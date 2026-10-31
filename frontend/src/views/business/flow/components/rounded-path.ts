/**
 * 正交折线 → 圆角折线（P12 视觉升级）。
 *
 * `graph.ts::edgePath` 产出的是 `M x y L x y L ...` 的正交折线（坐标口径的唯一来源，
 * 本卡不动它）；这里做的是**纯字符串层面**的后处理：把每个中间拐点切成一段圆角，
 * 让连线在拐角处不再「硬折 90°」。
 *
 * 为什么不改 `graph.ts`：那边是连线起终点 / 命中测试 / 自动分层的共同底座，
 * 630 行、被 29 个单测锁着；圆角只是画法，属于渲染层。
 *
 * 抽成独立纯函数的好处：可以在 `tsx --test` 里直接断言 `d` 里有几个 `Q`、半径有没有超段长。
 */

/** 画布坐标点 */
export interface PathPoint {
  /** x */
  x: number;
  /** y */
  y: number;
}

/** 圆角半径上限（px，画布逻辑坐标） */
export const ROUND_RADIUS = 8;

/** 数字保留两位小数（`d` 短一点，DOM 属性也干净） */
function num(value: number): number {
  return Math.round(value * 100) / 100;
}

/**
 * 解析正交折线的 `d`（只认**绝对坐标**的 `M` / `L`，`edgePath` 只产出这两种）。
 *
 * 解析失败（非法字符 / 参数不足）时返回已解析到的点，绝不抛错 ——
 * 画布上有一条脏 `d` 不该让整个流程图白屏。
 *
 * # 参数
 * - `d`：`M 0 0 L 10 0 L 10 20` 这种字符串
 */
export function parsePathPoints(d: string): PathPoint[] {
  const points: PathPoint[] = [];
  if (!d) {
    return points;
  }
  const pattern = /([ML])\s*(-?\d+(?:\.\d+)?)[\s,]+(-?\d+(?:\.\d+)?)/g;
  let match: RegExpExecArray | null = pattern.exec(d);
  while (match) {
    const x = Number(match[2]);
    const y = Number(match[3]);
    if (Number.isFinite(x) && Number.isFinite(y)) {
      points.push({ x, y });
    }
    match = pattern.exec(d);
  }
  return points;
}

/**
 * 把正交折线点序列转成「圆角折线」的 `d`。
 *
 * 规则（每个**中间点**）：
 * - 进出方向都是水平或都是垂直（共线）→ 不切圆角，直接 `L` 过去；
 * - 否则半径 `r = min(radius, 入段长/2, 出段长/2)`（短段自动缩小，不会切过头），
 *   先 `L` 到拐点前 `r` 处，再用一条二次贝塞尔 `Q` 以拐点作控制点圆过去。
 *
 * # 参数
 * - `points`：折线点序列（≥2 个点）
 * - `radius`：圆角半径上限，默认 {@link ROUND_RADIUS}
 *
 * # 返回值
 * 合法的 SVG `d`：点不足两个时返回空串（不产生 `NaN` / 非法路径）。
 */
export function buildRoundedPath(points: PathPoint[], radius: number = ROUND_RADIUS): string {
  if (points.length === 0) {
    return '';
  }
  const first = points[0];
  const parts: string[] = [`M ${num(first.x)} ${num(first.y)}`];
  if (points.length === 1) {
    // 单点：给一条零长度线，`d` 依然合法（避免上层拿到空串后 path 不渲染）
    parts.push(`L ${num(first.x)} ${num(first.y)}`);
    return parts.join(' ');
  }

  const cap = Math.max(0, radius);
  for (let i = 1; i < points.length - 1; i += 1) {
    const prev = points[i - 1];
    const cur = points[i];
    const next = points[i + 1];
    const inLen = Math.hypot(cur.x - prev.x, cur.y - prev.y);
    const outLen = Math.hypot(next.x - cur.x, next.y - cur.y);
    // 重合点：跳过圆角（方向没有定义）
    if (inLen < 0.001 || outLen < 0.001) {
      parts.push(`L ${num(cur.x)} ${num(cur.y)}`);
      continue;
    }
    const sx = (cur.x - prev.x) / inLen;
    const sy = (cur.y - prev.y) / inLen;
    const tx = (next.x - cur.x) / outLen;
    const ty = (next.y - cur.y) / outLen;
    // 共线（同向或反向）：没有拐角，不做圆角
    if (Math.abs(sx * tx + sy * ty) > 0.999) {
      parts.push(`L ${num(cur.x)} ${num(cur.y)}`);
      continue;
    }
    const r = Math.min(cap, inLen / 2, outLen / 2);
    if (r < 0.5) {
      parts.push(`L ${num(cur.x)} ${num(cur.y)}`);
      continue;
    }
    parts.push(`L ${num(cur.x - sx * r)} ${num(cur.y - sy * r)}`);
    parts.push(`Q ${num(cur.x)} ${num(cur.y)} ${num(cur.x + tx * r)} ${num(cur.y + ty * r)}`);
  }

  const last = points[points.length - 1];
  parts.push(`L ${num(last.x)} ${num(last.y)}`);
  return parts.join(' ');
}

/** `edgePath` 的 `d` → 圆角折线 `d`（画布里的唯一调用入口） */
export function roundedEdgePath(d: string, radius: number = ROUND_RADIUS): string {
  const points = parsePathPoints(d);
  if (points.length < 2) {
    return d;
  }
  return buildRoundedPath(points, radius);
}

/**
 * 估算边标签的文字宽度（SVG 没有自动排版，标签底色要自己量）。
 *
 * CJK 按 1 个字宽、ASCII 按 0.55 个字宽估；字号见 `.flow-edge__label text`（10.5 px）。
 *
 * # 参数
 * - `label`：标签文案
 * - `fontSize`：字号（默认 10.5）
 */
export function labelTextWidth(label: string, fontSize = 10.5): number {
  let units = 0;
  for (const char of label) {
    units += /[\u4e00-\u9fa5\u3000-\u303f\uff00-\uffef]/.test(char) ? 1 : 0.55;
  }
  return Math.ceil(units * fontSize);
}
