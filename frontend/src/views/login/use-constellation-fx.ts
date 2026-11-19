/**
 * 登录页左侧品牌区的「星座连线」鼠标特效。
 *
 * ## 为什么用 canvas 而不是 DOM
 * 56 个点 + 两点之间的距离遍历（O(n²) ≈ 1540 次/帧）。用 DOM 元素做的话，
 * 每帧要改 1500+ 个 style，浏览器重排直接掉帧。canvas 一次 draw 就够。
 *
 * ## 交互语义
 * 点本身以极慢速度漂移（0.22px/帧，看着几乎静止），**鼠标是「第 0 号点」**：
 *   · 附近 190px 内的点被吸过来（力随距离衰减，边缘为 0，不会突变）
 *   · 与鼠标的距离 < 200px 的点连金线（越近越亮）
 * 所以不动鼠标时画面很安静，一动就有响应 —— 这是用户明确要的效果，
 * 不是「自动播放的动画」。
 *
 * ## 边界处理
 * - `prefers-reduced-motion: reduce` → 完全不启动，只留静态渐变底
 * - 页面切到后台 → 暂停 rAF（省电、避免回来时跳帧）
 * - 组件卸载 / 尺寸变化 → cancelAnimationFrame + 重新测算 DPR
 * - 移动端 → touchmove 同样驱动（否则手机上等于没有）
 */

import { onBeforeUnmount, onMounted, type Ref } from 'vue';

const BLUE = '111,138,184'; // 雾蓝（主题 --app-brand-strong 同色系）
const GOLD = '193,172,123'; // 流金（主题里的漂木棕点缀）

/** 一个漂移的点 */
interface Particle {
  x: number;
  y: number;
  vx: number;
  vy: number;
  r: number;
  gold: boolean;
}

/** 点的数量。56 是实测在 560×620 容器里既不空也不糊的量 */
const PARTICLE_COUNT = 56;
/** 点与点连线的最大距离 */
const LINK_DIST = 132;
/** 鼠标的影响半径（吸引 + 连线） */
const MOUSE_RADIUS = 190;
const MOUSE_LINK_DIST = 200;

export interface ConstellationFx {
  /** 绑到宿主元素的 @mousemove */
  onMouseMove: (e: MouseEvent) => void;
  /** 绑到宿主元素的 @mouseleave */
  onMouseLeave: () => void;
  /** 绑到宿主元素的 @touchmove */
  onTouchMove: (e: TouchEvent) => void;
}

/**
 * @param canvasRef canvas 元素
 * @param hostRef   承载 canvas 的品牌区（用来算鼠标相对坐标）
 */
export function useConstellationFx(
  canvasRef: Ref<HTMLCanvasElement | null>,
  hostRef: Ref<HTMLElement | null>,
): ConstellationFx {
  let ctx: CanvasRenderingContext2D | null = null;
  let particles: Particle[] = [];
  let raf = 0;
  let observer: ResizeObserver | null = null;
  let running = false;

  let W = 0;
  let H = 0;
  let DPR = 1;

  const mouse = { x: -9999, y: -9999, active: false };

  // ---------- 初始化 ----------

  function spawnParticles() {
    particles = Array.from({ length: PARTICLE_COUNT }, () => ({
      x: Math.random() * W,
      y: Math.random() * H,
      vx: (Math.random() - 0.5) * 0.22,
      vy: (Math.random() - 0.5) * 0.22,
      r: Math.random() * 1.4 + 1.1,
      gold: Math.random() < 0.18,
    }));
  }

  function resize() {
    const canvas = canvasRef.value;
    const host = hostRef.value;
    if (!canvas || !host) {
      return;
    }
    const rect = host.getBoundingClientRect();
    DPR = Math.min(window.devicePixelRatio || 1, 2); // 上限 2：3x 屏上没必要，省一半像素
    W = rect.width;
    H = rect.height;
    canvas.width = Math.round(W * DPR);
    canvas.height = Math.round(H * DPR);
    ctx = canvas.getContext('2d');
    if (ctx) {
      ctx.setTransform(DPR, 0, 0, DPR, 0, 0);
    }
    spawnParticles();
  }

  // ---------- 绘制 ----------

  function step() {
    if (!ctx) {
      return;
    }
    ctx.clearRect(0, 0, W, H);

    // 1. 漂移（出界就绕到对面，避免边界处点堆积）
    for (const p of particles) {
      p.x += p.vx;
      p.y += p.vy;
      if (p.x < -10) {
        p.x = W + 10;
      } else if (p.x > W + 10) {
        p.x = -10;
      }
      if (p.y < -10) {
        p.y = H + 10;
      } else if (p.y > H + 10) {
        p.y = -10;
      }
    }

    // 2. 鼠标引力：力 ∝ (1 - d/R)，边缘正好为 0，不会突然"跳"
    if (mouse.active) {
      for (const p of particles) {
        const dx = mouse.x - p.x;
        const dy = mouse.y - p.y;
        const d2 = dx * dx + dy * dy;
        if (d2 < MOUSE_RADIUS * MOUSE_RADIUS && d2 > 1) {
          const d = Math.sqrt(d2);
          const f = (1 - d / MOUSE_RADIUS) * 0.55;
          p.x += (dx / d) * f;
          p.y += (dy / d) * f;
        }
      }
    }

    // 3. 点与点连线
    for (let i = 0; i < particles.length; i++) {
      for (let j = i + 1; j < particles.length; j++) {
        const a = particles[i];
        const b = particles[j];
        const d = Math.hypot(a.x - b.x, a.y - b.y);
        if (d < LINK_DIST) {
          ctx.strokeStyle = `rgba(${BLUE},${((1 - d / LINK_DIST) * 0.34).toFixed(3)})`;
          ctx.lineWidth = 0.8;
          ctx.beginPath();
          ctx.moveTo(a.x, a.y);
          ctx.lineTo(b.x, b.y);
          ctx.stroke();
        }
      }
    }

    // 4. 鼠标 → 附近点（金线，更亮，让鼠标成为视觉焦点）
    if (mouse.active) {
      for (const p of particles) {
        const d = Math.hypot(p.x - mouse.x, p.y - mouse.y);
        if (d < MOUSE_LINK_DIST) {
          ctx.strokeStyle = `rgba(${GOLD},${((1 - d / MOUSE_LINK_DIST) * 0.7).toFixed(3)})`;
          ctx.lineWidth = 1;
          ctx.beginPath();
          ctx.moveTo(mouse.x, mouse.y);
          ctx.lineTo(p.x, p.y);
          ctx.stroke();
        }
      }

      // 鼠标光晕
      const glow = ctx.createRadialGradient(mouse.x, mouse.y, 0, mouse.x, mouse.y, 46);
      glow.addColorStop(0, `rgba(${GOLD},.5)`);
      glow.addColorStop(1, `rgba(${GOLD},0)`);
      ctx.fillStyle = glow;
      ctx.beginPath();
      ctx.arc(mouse.x, mouse.y, 46, 0, Math.PI * 2);
      ctx.fill();
    }

    // 5. 画点
    for (const p of particles) {
      ctx.fillStyle = p.gold ? `rgba(${GOLD},.85)` : `rgba(${BLUE},.72)`;
      ctx.beginPath();
      ctx.arc(p.x, p.y, p.r, 0, Math.PI * 2);
      ctx.fill();
    }

    if (running) {
      raf = requestAnimationFrame(step);
    }
  }

  function start() {
    if (running) {
      return;
    }
    running = true;
    raf = requestAnimationFrame(step);
  }

  function stop() {
    running = false;
    if (raf) {
      cancelAnimationFrame(raf);
      raf = 0;
    }
  }

  // ---------- 事件 ----------

  function toLocal(clientX: number, clientY: number) {
    const host = hostRef.value;
    if (!host) {
      return;
    }
    const rect = host.getBoundingClientRect();
    mouse.x = clientX - rect.left;
    mouse.y = clientY - rect.top;
    mouse.active = true;
  }

  const onMouseMove = (e: MouseEvent) => toLocal(e.clientX, e.clientY);

  const onMouseLeave = () => {
    mouse.active = false;
    mouse.x = -9999;
    mouse.y = -9999;
  };

  const onTouchMove = (e: TouchEvent) => {
    const t = e.touches[0];
    if (t) {
      toLocal(t.clientX, t.clientY);
    }
  };

  // ---------- 生命周期 ----------

  function onVisibility() {
    if (document.hidden) {
      stop();
    } else {
      start();
    }
  }

  onMounted(() => {
    // 无障碍：用户明确要求减少动效时，一张静态渐变底就够
    if (window.matchMedia('(prefers-reduced-motion: reduce)').matches) {
      return;
    }

    const host = hostRef.value;
    if (!host) {
      return;
    }
    resize();
    start();

    observer = new ResizeObserver(() => {
      resize();
    });
    observer.observe(host);

    document.addEventListener('visibilitychange', onVisibility);
  });

  onBeforeUnmount(() => {
    stop();
    observer?.disconnect();
    observer = null;
    document.removeEventListener('visibilitychange', onVisibility);
  });

  return { onMouseMove, onMouseLeave, onTouchMove };
}
