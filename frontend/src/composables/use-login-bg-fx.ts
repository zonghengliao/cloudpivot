/**
 * 登录页背景网格特效（方格 40 / 六边形中 R=26）。
 *
 * ## 为什么单独抽出来
 * 登录页已经有一个「品牌区星座连线」canvas，再往里塞网格动画会让 index.vue
 * 继续膨胀（它已经 500+ 行）。这里把网格的建格、脉冲、遮罩、呼吸全封在
 * 一个 composable 里，页面只负责挂 canvas + 底部切换按钮。
 *
 * ## 效果
 * - **底**：连续网格（方格）或尖顶蜂窝（六边形），线色取 `--td-component-border`
 * - **脉冲**：每 0.6~1.2 秒起一波，一波 18~42 格，最多 2 波并行。
 *   单格像呼吸：涨满（smoothstep）→ 停 → 退掉，一轮 4~7 秒。
 * - **遮罩**：径向从中心向外淡出，和原来 CSS 的 mask-image 同口径
 * - **整体呼吸**：14 秒一轮，opacity 0.58 → 0.95（与旧版 login-bg-breath 一致）
 *
 * ## 边界
 * - `prefers-reduced-motion: reduce` → 只画静态网格，不跑脉冲
 * - 页面切后台 → 暂停 rAF
 * - 卸载 → 清 rAF + ResizeObserver + 事件
 * - 颜色每秒重读一次 CSS 变量 → 切配色不用刷新
 */

import { onBeforeUnmount, onMounted, ref, type Ref } from 'vue';

export type LoginBgShape = 'square' | 'hex';

/** 两种形状的尺寸（与设计稿一致：方格 40、六边形 R=26） */
const SHAPE_PRESET: Record<LoginBgShape, { gap?: number; r?: number }> = {
  square: { gap: 40 },
  hex: { r: 26 },
};

const WAVE_MIN = 18;
const WAVE_MAX = 42;
const MAX_WAVES = 2;
const FPS = 60;

interface Cell {
  kind: LoginBgShape;
  x: number;
  y: number;
  /** 方格边长 / 六边形外接圆半径 */
  s: number;
}

interface Pick {
  idx: number;
  delay: number;
  gold: boolean;
  peak: number;
}

interface Wave {
  picks: Pick[];
  start: number;
  dur: number;
}

export interface LoginBgFx {
  /** 当前形状（只读） */
  shape: Readonly<Ref<LoginBgShape>>;
  /** 切换形状 */
  setShape: (s: LoginBgShape) => void;
}

export function useLoginBgFx(
  canvasRef: Ref<HTMLCanvasElement | null>,
  hostRef: Ref<HTMLElement | null>,
  initial: LoginBgShape = 'hex',
): LoginBgFx {
  let ctx: CanvasRenderingContext2D | null = null;
  let W = 0;
  let H = 0;
  let DPR = 1;

  let cells: Cell[] = [];
  let waves: Wave[] = [];
  let nextWaveAt = 0;
  let t = 0;
  let raf = 0;
  let running = false;
  let observer: ResizeObserver | null = null;

  // 颜色缓存（每秒重读）
  let C_BORDER = [170, 183, 194];
  let C_BRAND = [111, 138, 184];
  let C_ACCENT = [193, 172, 123];
  let colorTick = 0;

  const shape = ref<LoginBgShape>(initial);

  const rgba = (c: number[], a: number) => `rgba(${c[0]},${c[1]},${c[2]},${a})`;
  const smooth = (x: number) => x * x * (3 - 2 * x);

  /** 从 CSS 变量读颜色 → [r,g,b] */
  function readRgb(varName: string, fallback: number[]): number[] {
    if (typeof document === 'undefined') return fallback;
    const raw = getComputedStyle(document.documentElement).getPropertyValue(varName).trim();
    if (!raw) return fallback;
    const hex = raw.replace('#', '');
    if (/^[0-9a-fA-F]{6}$/.test(hex)) {
      return [
        parseInt(hex.slice(0, 2), 16),
        parseInt(hex.slice(2, 4), 16),
        parseInt(hex.slice(4, 6), 16),
      ];
    }
    const m = raw.match(/rgba?\(([^)]+)\)/);
    if (m) return m[1].split(',').slice(0, 3).map((v) => Math.round(parseFloat(v)));
    return fallback;
  }

  function refreshColors() {
    C_BORDER = readRgb('--td-component-border', C_BORDER);
    C_BRAND = readRgb('--app-brand', C_BRAND);
    C_ACCENT = readRgb('--app-brand-accent', C_ACCENT);
  }

  // ---------- 建格 ----------
  function build() {
    const preset = SHAPE_PRESET[shape.value];
    cells = [];
    waves = [];
    nextWaveAt = 0;

    if (shape.value === 'square') {
      const GAP = preset.gap ?? 40;
      const cols = Math.ceil(W / GAP) + 1;
      const rows = Math.ceil(H / GAP) + 1;
      for (let i = 0; i < cols; i++) {
        for (let j = 0; j < rows; j++) {
          cells.push({ kind: 'square', x: i * GAP, y: j * GAP, s: GAP });
        }
      }
    } else {
      // 尖顶六边形（pointy-top）
      const R = preset.r ?? 26;
      const hx = Math.sqrt(3) * R;
      const vy = 1.5 * R;
      const cols = Math.ceil(W / hx) + 2;
      const rows = Math.ceil(H / vy) + 2;
      for (let j = -1; j < rows; j++) {
        for (let i = -1; i < cols; i++) {
          cells.push({
            kind: 'hex',
            x: i * hx + (j % 2 ? hx / 2 : 0),
            y: j * vy,
            s: R,
          });
        }
      }
    }
  }

  /** 给格子上路径（调用方决定 stroke / fill） */
  function pathOf(c: Cell) {
    if (!ctx) return;
    ctx.beginPath();
    if (c.kind === 'square') {
      ctx.rect(c.x + 0.5, c.y + 0.5, c.s - 1, c.s - 1);
    } else {
      for (let k = 0; k < 6; k++) {
        const a = (Math.PI / 180) * (60 * k - 30);
        const px = c.x + c.s * Math.cos(a);
        const py = c.y + c.s * Math.sin(a);
        if (k === 0) ctx.moveTo(px, py);
        else ctx.lineTo(px, py);
      }
      ctx.closePath();
    }
  }

  // ---------- 波 ----------
  function spawnWave() {
    if (!cells.length) return;
    const count = WAVE_MIN + Math.floor(Math.random() * (WAVE_MAX - WAVE_MIN + 1));
    const picks: Pick[] = [];
    for (let k = 0; k < count; k++) {
      picks.push({
        idx: Math.floor(Math.random() * cells.length),
        delay: Math.random() * 0.25,
        gold: Math.random() < 0.1,
        peak: 0.14 + Math.random() * 0.1,
      });
    }
    waves.push({ picks, start: t, dur: 240 + Math.random() * 180 });
    nextWaveAt = 0;
  }

  function updateWaves() {
    const before = waves.length;
    waves = waves.filter((w) => t - w.start <= w.dur);
    if (waves.length < before) {
      nextWaveAt = t + 36 + Math.random() * 36; // 0.6~1.2 秒
    }
    if (waves.length < MAX_WAVES && t >= nextWaveAt) {
      spawnWave();
    }
  }

  /** 某格在某一波里的实心度 0~1 */
  function fillOf(w: Wave, p: Pick): number {
    const elapsed = (t - w.start) / w.dur;
    if (elapsed < p.delay) return 0;
    const local = (elapsed - p.delay) / (1 - p.delay);
    if (local > 1) return 0;
    if (local < 0.35) return smooth(local / 0.35); // 涨
    if (local < 0.55) return 1; // 停
    return smooth(1 - (local - 0.55) / 0.45); // 退
  }

  // ---------- 绘制 ----------
  function drawStatic() {
    if (!ctx) return;
    ctx.clearRect(0, 0, W, H);
    if (shape.value === 'square') {
      const GAP = SHAPE_PRESET.square.gap ?? 40;
      ctx.strokeStyle = rgba(C_BORDER, 0.2);
      ctx.lineWidth = 1;
      ctx.beginPath();
      for (let x = 0; x <= W; x += GAP) {
        ctx.moveTo(x + 0.5, 0);
        ctx.lineTo(x + 0.5, H);
      }
      for (let y = 0; y <= H; y += GAP) {
        ctx.moveTo(0, y + 0.5);
        ctx.lineTo(W, y + 0.5);
      }
      ctx.stroke();
    } else {
      ctx.strokeStyle = rgba(C_BORDER, 0.11);
      ctx.lineWidth = 1;
      for (const c of cells) {
        if (c.x < -c.s || c.x > W + c.s || c.y < -c.s || c.y > H + c.s) continue;
        pathOf(c);
        ctx.stroke();
      }
    }
    applyMask();
  }

  function applyMask() {
    if (!ctx) return;
    ctx.globalCompositeOperation = 'destination-in';
    const mask = ctx.createRadialGradient(
      W / 2,
      H / 2,
      Math.min(W, H) * 0.18,
      W / 2,
      H / 2,
      Math.max(W, H) * 0.62,
    );
    mask.addColorStop(0, 'rgba(0,0,0,1)');
    mask.addColorStop(0.55, 'rgba(0,0,0,0.92)');
    mask.addColorStop(1, 'rgba(0,0,0,0)');
    ctx.fillStyle = mask;
    ctx.fillRect(0, 0, W, H);
    ctx.globalCompositeOperation = 'source-over';
  }

  function step() {
    if (!ctx) return;
    t++;
    if (colorTick++ % 60 === 0) refreshColors();

    ctx.clearRect(0, 0, W, H);

    // 14 秒整体呼吸（与旧版 CSS 动画同口径）
    const breath = 0.58 + 0.37 * (0.5 + 0.5 * Math.sin((t / FPS) * ((2 * Math.PI) / 14)));

    ctx.save();
    ctx.globalAlpha = breath;

    // ① 网格
    if (shape.value === 'square') {
      const GAP = SHAPE_PRESET.square.gap ?? 40;
      ctx.strokeStyle = rgba(C_BORDER, 0.2);
      ctx.lineWidth = 1;
      ctx.beginPath();
      for (let x = 0; x <= W; x += GAP) {
        ctx.moveTo(x + 0.5, 0);
        ctx.lineTo(x + 0.5, H);
      }
      for (let y = 0; y <= H; y += GAP) {
        ctx.moveTo(0, y + 0.5);
        ctx.lineTo(W, y + 0.5);
      }
      ctx.stroke();
    } else {
      ctx.strokeStyle = rgba(C_BORDER, 0.11);
      ctx.lineWidth = 1;
      for (const c of cells) {
        if (c.x < -c.s || c.x > W + c.s || c.y < -c.s || c.y > H + c.s) continue;
        pathOf(c);
        ctx.stroke();
      }
    }

    // ② 脉冲
    updateWaves();
    const best = new Map<number, { k: number; gold: boolean; peak: number }>();
    for (const w of waves) {
      for (const p of w.picks) {
        const k = fillOf(w, p);
        if (k < 0.02) continue;
        const prev = best.get(p.idx);
        if (!prev || k > prev.k) best.set(p.idx, { k, gold: p.gold, peak: p.peak });
      }
    }
    for (const [idx, v] of best) {
      const cell = cells[idx];
      if (!cell) continue;
      const color = v.gold ? C_ACCENT : C_BRAND;
      pathOf(cell);
      ctx.fillStyle = rgba(color, v.k * v.peak);
      ctx.fill();
      if (v.k > 0.45) {
        ctx.strokeStyle = rgba(color, (v.k - 0.45) * 0.7);
        ctx.lineWidth = 1.2;
        pathOf(cell);
        ctx.stroke();
      }
    }

    // ③ 径向遮罩
    applyMask();
    ctx.restore();

    if (running) raf = requestAnimationFrame(step);
  }

  // ---------- 生命周期 ----------
  function resize() {
    const canvas = canvasRef.value;
    const host = hostRef.value;
    if (!canvas || !host) return;
    const rect = host.getBoundingClientRect();
    DPR = Math.min(window.devicePixelRatio || 1, 2);
    W = rect.width;
    H = rect.height;
    canvas.width = Math.round(W * DPR);
    canvas.height = Math.round(H * DPR);
    ctx = canvas.getContext('2d');
    if (ctx) ctx.setTransform(DPR, 0, 0, DPR, 0, 0);
    refreshColors();
    build();
    drawStatic();
  }

  function start() {
    if (running) return;
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

  function setShape(s: LoginBgShape) {
    if (shape.value === s) return;
    shape.value = s;
    build();
    if (!running) drawStatic();
  }

  function onVisibility() {
    if (document.hidden) stop();
    else start();
  }

  onMounted(() => {
    const host = hostRef.value;
    if (!host) return;

    resize();

    if (window.matchMedia('(prefers-reduced-motion: reduce)').matches) {
      // 只画静态网格
      return;
    }
    start();

    observer = new ResizeObserver(() => resize());
    observer.observe(host);
    document.addEventListener('visibilitychange', onVisibility);
  });

  onBeforeUnmount(() => {
    stop();
    observer?.disconnect();
    observer = null;
    document.removeEventListener('visibilitychange', onVisibility);
  });

  return { shape, setShape };
}
