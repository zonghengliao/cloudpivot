/**
 * 甘特依赖连线的**纯几何 + 纯文案**（不依赖 Vue / DOM，单测直接跑）。
 *
 * # 为什么要单独拆一份
 *
 * 依赖线的形状有三个坑，坑都在「换算」这一层，不在模板里：
 *
 * 1. **FS 型两端的锚点**：前置取「右端中点」、后继取「左端中点」（结束 → 开始）。
 *    锚点必须是**拖动中的实时几何**（`barGeom`），所以这里只收几何量、不收日期 ——
 *    拖动预演与落库后走的是同一条路径，线才不会跳。
 * 2. **后继排在前置左边**（乱序排期）：标准的「右→下→左」折线会横穿后继条形，
 *    必须绕行走行间隙（两条条形中间的水平带），且**任何一段都不许伸进左侧任务名栏**
 *    （`minX` = 左栏宽度 + 2；不钳的话线会画到任务名上）。
 * 3. **命中区不能盖住箭头尖**：箭头尖正好落在后继条形的左边缘，那里是「拖拽改期」
 *    的左手柄（8px 宽）。命中区（透明加粗路径）必须在最后一段之前收尾，
 *    否则悬停能显示但拖不动 —— 拖拽改期是硬红线，宁可不给悬停。
 *
 * # 依赖类型口径
 *
 * 库表 `biz_task_dependency.dep_type` 的值域是 `FS / SS / FF / SF`，
 * 后端甘特接口目前只下发 `FS` 边（SQL 硬过滤 `dep_type = 'FS'`）。
 * 所以前端对 `dep_type` / `lag_days` 一律**当可选读**：
 * 后端给了就按真值渲染，没给（老版本接口）就退化成 `FS / 0` —— 与接口的实际行为一致，
 * 不是猜的。
 */

/** 折线拐角的圆角半径（像素） */
const CORNER_R = 4;

/** 从条形端点探出去的水平短段（像素）：让箭头有一段「引线」，不和条形贴死 */
const STUB = 12;

/** 命中区的半宽（像素）：10px 描边 ≈ 手指/鼠标都能落上去，又不至于抢相邻行的事件 */
export const HIT_STROKE_WIDTH = 10;

/** 甘特条形与行的几何常量（与 gantt.vue 的 `BAR_H` / 条形上边距同源，测试里也用它算期望值） */
export interface LinkLayout {
  /** 左侧任务名栏宽度 */
  leftWidth: number;
  /** 前置所在行的行顶 y */
  predRowTop: number;
  /** 后继所在行的行顶 y */
  succRowTop: number;
  /** 条形上边距（行顶 + 该值 = 条形上沿） */
  barTopOffset: number;
  /** 条形高度 */
  barHeight: number;
  /** 画布右边界（钳制用，避免线伸到画布外） */
  maxX: number;
}

/** 一条依赖边要用的两端条形几何（`x` = 条形左沿，`width` = 条形宽度） */
export interface BarRect {
  x: number;
  width: number;
}

/** 一条依赖边的画法 */
export interface LinkGeometry {
  /** 可见路径（`<path d>`，带圆角折线） */
  d: string;
  /** 命中路径：去掉最后一段（避开后继条形左边缘的拖拽手柄） */
  dHit: string;
  /** 折线顶点（调试 / 断言用） */
  points: Array<{ x: number; y: number }>;
  /** 箭头尖（= 后继条形左边缘中点） */
  tip: { x: number; y: number };
  /** 悬停浮层锚点（竖段中点的右侧） */
  anchor: { x: number; y: number };
}

/** 数值钳制 */
function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

/** 四舍五入到 0.1（SVG 路径里不必带一长串浮点） */
function round(value: number): number {
  return Math.round(value * 10) / 10;
}

/**
 * 折线 → 带圆角的 SVG 路径。
 *
 * 每个拐角用二次贝塞尔（`Q 顶点, 圆角终点`）收圆：半径取 `min(4, 两段的一半)`，
 * 所以极短的段也不会画出反向的圆角。
 */
function roundedPath(points: Array<{ x: number; y: number }>): string {
  if (points.length === 0) {
    return '';
  }
  const first = points[0]!;
  if (points.length === 1) {
    return `M ${round(first.x)},${round(first.y)}`;
  }
  let d = `M ${round(first.x)},${round(first.y)}`;
  for (let index = 1; index < points.length - 1; index += 1) {
    const prev = points[index - 1]!;
    const curr = points[index]!;
    const next = points[index + 1]!;
    const inLen = Math.hypot(curr.x - prev.x, curr.y - prev.y);
    const outLen = Math.hypot(next.x - curr.x, next.y - curr.y);
    const radius = Math.min(CORNER_R, inLen / 2, outLen / 2);
    if (!(radius > 0.2)) {
      // 太短（或退化成零长）的段不值得收圆：直接走到顶点。
      // 零长段必须在这里拦下 —— 下面要按长度做除法，`0/0` 会让整条 `d` 变成 NaN。
      d += ` L ${round(curr.x)},${round(curr.y)}`;
      continue;
    }
    const inX = curr.x - ((curr.x - prev.x) / inLen) * radius;
    const inY = curr.y - ((curr.y - prev.y) / inLen) * radius;
    const outX = curr.x + ((next.x - curr.x) / outLen) * radius;
    const outY = curr.y + ((next.y - curr.y) / outLen) * radius;
    d += ` L ${round(inX)},${round(inY)} Q ${round(curr.x)},${round(curr.y)} ${round(outX)},${round(outY)}`;
  }
  const last = points[points.length - 1]!;
  d += ` L ${round(last.x)},${round(last.y)}`;
  return d;
}

/**
 * FS 依赖线几何：**前置条形右端中点 → 后继条形左端中点**。
 *
 * 三种走法（判据只看两端的水平关系，与行距无关）：
 *
 * | 情况 | 走法 |
 * |------|------|
 * | 后继在前置右侧且间隙够（`≥ 2*STUB`） | 右出 STUB → 竖直 → 水平进入后继左边缘 |
 * | 后继在前置右侧但很窄（`< 2*STUB`） | 右出到间隙中点 → 竖直 → 进入（不做无谓的绕行） |
 * | 后继排在前置左边（乱序） | 右出 STUB → 走到两条行之间 → 左折 → 竖直 → 进入 |
 *
 * 任何一段都钳在 `[leftWidth + 2, maxX]` 内：不伸进左侧任务名栏，也不出画布。
 */
export function buildFsLink(pred: BarRect, succ: BarRect, layout: LinkLayout): LinkGeometry {
  const minX = layout.leftWidth + 2;
  const y1 = round(layout.predRowTop + layout.barTopOffset + layout.barHeight / 2);
  const y2 = round(layout.succRowTop + layout.barTopOffset + layout.barHeight / 2);
  const x1 = round(clamp(pred.x + pred.width, minX, layout.maxX));
  const x2 = round(clamp(succ.x, minX, layout.maxX));
  const points: Array<{ x: number; y: number }> = [{ x: x1, y: y1 }];

  const gap = x2 - x1;
  if (y1 === y2) {
    // 同一条行（防御性分支，正常不会出现）：一条直线，不做无意义的竖段
    points.push({ x: x2, y: y1 });
  } else if (gap >= STUB * 2) {
    // 间隙充足：标准 FS 折线（竖段贴前置那一侧，一眼看出是「谁连到谁」）
    const midX = round(Math.min(x1 + STUB, Math.max(x2 - STUB, x1 + gap / 2)));
    points.push({ x: midX, y: y1 }, { x: midX, y: y2 }, { x: x2, y: y2 });
  } else if (gap > 0) {
    // 右侧但间隙窄：竖段放在间隙中点，避免「出去又回来」的抖动感
    const midX = round(x1 + gap / 2);
    points.push({ x: midX, y: y1 }, { x: midX, y: y2 }, { x: x2, y: y2 });
  } else {
    // 后继在前置左边：绕行到两行之间的空带，再从左侧进入（不横穿后继条形）
    const outX = round(clamp(x1 + STUB, minX, layout.maxX));
    const backX = round(clamp(x2 - STUB, minX, layout.maxX));
    const midY = round((y1 + y2) / 2);
    points.push(
      { x: outX, y: y1 },
      { x: outX, y: midY },
      { x: backX, y: midY },
      { x: backX, y: y2 },
      { x: x2, y: y2 },
    );
  }

  const tip = { x: x2, y: y2 };
  // 命中路径去掉最后一段：箭头尖正好落在后继条形的左边缘 = 拖拽改期的左手柄所在，
  // 命中区盖住它就会「能悬停但拖不动」。
  const hitPoints = points.length > 2 ? points.slice(0, -1) : points;
  return {
    d: roundedPath(points),
    dHit: roundedPath(hitPoints),
    points,
    tip,
    anchor: { x: points[1]?.x ?? x1, y: round((y1 + y2) / 2) },
  };
}

/** 依赖类型 → 中文名（`FS` = 完成-开始，是本版唯一会出现的类型） */
export const DEP_TYPE_LABEL: Record<string, string> = {
  FS: '完成-开始',
  SS: '开始-开始',
  FF: '完成-完成',
  SF: '开始-完成',
};

/** 依赖类型中文名（未知类型原样回显，不吞数据） */
export function depTypeLabel(depType: string | null | undefined): string {
  const key = (depType ?? 'FS').toUpperCase();
  return DEP_TYPE_LABEL[key] ?? key;
}

/**
 * 依赖是否还「卡着」后继。
 *
 * 口径与后端 `check_dependencies_ready` 一致：前置 `DONE` 算完成，
 * `CANCELLED` 视为已解除（核心设计「联动」表），其余状态都还在阻塞。
 */
export function depReleased(predStatus: string | null | undefined): boolean {
  return predStatus === 'DONE' || predStatus === 'CANCELLED';
}

/** 悬停浮层要显示的依赖信息（全部来自接口，不在前端推算日期） */
export interface LinkTipInput {
  /** 依赖类型 `FS`（缺省按 FS） */
  depType?: string | null;
  /** 滞后天数（缺省 0；只是展示，不参与排程） */
  lagDays?: number | null;
  /** 前置任务 */
  pred: { taskNo: string; title: string };
  /** 后继任务 */
  succ: { taskNo: string; title: string };
  /** 前置是否已完成 / 已取消（= 依赖已解除，不再阻塞后继） */
  released: boolean;
}

/** 悬停浮层的文案（第一行就是依赖类型 —— 卡面要求「悬停显示依赖类型」） */
export function linkTipLines(input: LinkTipInput): string[] {
  const depType = (input.depType ?? 'FS').toUpperCase();
  const lag = Number.isFinite(input.lagDays) ? Number(input.lagDays) : 0;
  const state = input.released ? '前置已完成 / 已取消（依赖已解除）' : '前置未完成（后继被阻塞）';
  return [
    `依赖类型：${depTypeLabel(depType)}（${depType}）`,
    `前置 ${input.pred.taskNo} ${input.pred.title}`,
    `后继 ${input.succ.taskNo} ${input.succ.title}`,
    lag > 0 ? `滞后 ${lag} 天 · ${state}` : state,
  ];
}
