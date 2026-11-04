/**
 * 流程画布引擎开关（feature flag）。
 *
 * 画布有两套实现，**契约完全一致**：
 * - `vueflow`：`FlowCanvasVueFlow.vue`（Vue Flow 基座，**默认**）
 * - `native`：`FlowCanvas.vue`（纯 SVG 自绘，老画布，**已 deprecated**，只保留作回退）
 *
 * 切换优先级（高 → 低）：
 * 1. URL 参数 `?canvas=native|vueflow`（开发 / 灰度 / 线上回退用，刷新即生效）
 * 2. 构建期环境变量 `VITE_FLOW_CANVAS=native|vueflow`（可随部署灰一整个环境）
 * 3. 默认 `vueflow`
 *
 * # 为什么默认切到 vueflow（VUE-FLOW-3）
 *
 * 换库的初衷是 Safari/WebKit 下的画布渲染与拖拽缺陷（老画布 `<g transform>` +
 * `foreignObject` 的绘制、拖拽被当成选中文本吃指针事件）。VUE-FLOW-1/2/2B 已把
 * 9 种节点、11 项业务特性、锚点方向、落点容差逐项对齐并用 Chrome + WebKit 双引擎
 * 回归过，本卡再把默认值反过来。
 *
 * # 回退方法（至少保留一个版本，Ops 一秒钟就能切回）
 *
 * - 单个页面：URL 加 `?canvas=native` 刷新即可（不改库、不发版）；
 * - 整个环境：构建时 `VITE_FLOW_CANVAS=native pnpm build`（或部署环境变量里置 native）；
 * - 永久回退：把本文件 `resolveFlowCanvasEngine()` 末行的 `'vueflow'` 改回 `'native'`。
 *
 * 回退开关的可用性由 `tests/unit/flow-canvas-vueflow-flag.test.ts` ①③④ 钉住。
 *
 * # 为什么参数优先于 env
 *
 * env 是「发布时定死」的运维手段，参数是「当场验证」的调试手段 ——
 * 运维灰度一开后，开发者仍需要能对单个页面做 A/B（回归对比、缺陷复现）。
 */

/** 画布引擎 */
export type FlowCanvasEngine = 'native' | 'vueflow';

/** URL 参数名 */
export const FLOW_CANVAS_PARAM = 'canvas';

/** 构建期环境变量名 */
export const FLOW_CANVAS_ENV_VAR = 'VITE_FLOW_CANVAS';

/** 把任意输入归一成引擎名（不认识的一律 null，交给下一级兜底） */
export function normalizeFlowCanvasEngine(value: unknown): FlowCanvasEngine | null {
  if (typeof value !== 'string') {
    return null;
  }
  const normalized = value.trim().toLowerCase();
  if (normalized === 'vueflow' || normalized === 'vue-flow') {
    return 'vueflow';
  }
  if (normalized === 'native' || normalized === 'svg') {
    return 'native';
  }
  return null;
}

/** 从 query string 里取引擎名（没有 / 非法 → null） */
export function engineFromSearch(search: string): FlowCanvasEngine | null {
  const query = search.startsWith('?') ? search.slice(1) : search;
  if (!query) {
    return null;
  }
  for (const pair of query.split('&')) {
    const [rawKey, rawValue = ''] = pair.split('=');
    if (decodeURIComponent(rawKey) === FLOW_CANVAS_PARAM) {
      return normalizeFlowCanvasEngine(decodeURIComponent(rawValue));
    }
  }
  return null;
}

/**
 * 解析当前应该用哪套画布。
 *
 * # 参数
 * - `search`：URL query（默认取 `window.location.search`；单测直接传值）
 * - `env`：构建期变量（默认取 `import.meta.env.VITE_FLOW_CANVAS`）
 */
export function resolveFlowCanvasEngine(options: { search?: string; env?: unknown } = {}): FlowCanvasEngine {
  const search = options.search ?? (typeof window === 'undefined' ? '' : window.location.search);
  // `import.meta.env` 只在 Vite 构建里存在（单测走 tsx 时是 undefined），这里取值要防一手
  const envSource = ((import.meta as unknown as { env?: Record<string, unknown> }).env ?? {}) as Record<string, unknown>;
  const env = options.env ?? envSource[FLOW_CANVAS_ENV_VAR];
  return engineFromSearch(search) ?? normalizeFlowCanvasEngine(env) ?? 'vueflow';
}
