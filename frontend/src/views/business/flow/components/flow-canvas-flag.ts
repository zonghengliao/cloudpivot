/**
 * 流程画布引擎开关（feature flag）。
 *
 * 画布有两套实现，**契约完全一致**：
 * - `native`：`FlowCanvas.vue`（纯 SVG 自绘，老画布，默认）
 * - `vueflow`：`FlowCanvasVueFlow.vue`（Vue Flow 基座）
 *
 * 切换优先级（高 → 低）：
 * 1. URL 参数 `?canvas=vueflow|native`（开发 / 灰度验证用，刷新即生效）
 * 2. 构建期环境变量 `VITE_FLOW_CANVAS=vueflow|native`（可随部署灰一整个环境）
 * 3. 默认 `native`
 *
 * # 为什么默认还是 native
 *
 * 新画布要等 VUE-FLOW-3 真机全量验证后再改默认；在那之前用户随时能回退，
 * 不用改代码、不用重发版（URL 参数即可切换）。
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
  return engineFromSearch(search) ?? normalizeFlowCanvasEngine(env) ?? 'native';
}
