/**
 * `e2e/target-guard.mjs` 的形状声明。
 *
 * 那个模块刻意保持「零构建的纯 ESM」（Playwright 配置、`node e2e/target-guard.mjs --check`
 * 命令行、`tests/unit/target-guard.test.ts` 三处共用），所以不改成 `.ts`；
 * TS 侧的出口只在这里声明 —— 用 `.d.mts` 与 `.mjs` 同目录，`import '../../e2e/target-guard.mjs'`
 * 能按 NodeNext 规则直接解析到它，不需要 `declare module '*.mjs'` 那种通配兜底。
 */

/** 目标判定结论（`classifyTarget` 的返回值） */
export interface TargetVerdict {
  /** 是否判定为隔离实例 */
  isolated: boolean;
  /** 归一化后的目标 URL */
  url: string;
  /** 主机名（小写） */
  host: string;
  /** 端口（缺省按协议补 80/443） */
  port: string;
  /** 判定理由（人话，直接进日志 / 异常文案） */
  reason: string;
  /** 仅 escape hatch 放行时为 true */
  overridden?: boolean;
}

export interface AssertTargetOptions {
  /** 留痕文件根目录（默认 process.cwd()） */
  projectRoot?: string;
  /** 日志出口（默认 console） */
  logger?: {
    log: (message: string) => unknown;
    warn: (message: string) => unknown;
  };
  /** 放行 prod 时是否写 `e2e/.auth/prod-target.flag`（默认 true） */
  writeFlag?: boolean;
}

/** 判定目标（纯函数，不做 IO） */
export function classifyTarget(rawUrl: unknown): TargetVerdict;

/** 硬判据：非隔离目标直接抛错（Playwright setup 阶段就红） */
export function assertIsolatedTarget(rawUrl: unknown, options?: AssertTargetOptions): TargetVerdict;

/** 跑完 prod 后的检查清单（原样返回模块内的步骤串） */
export function postrunSteps(): string[];
