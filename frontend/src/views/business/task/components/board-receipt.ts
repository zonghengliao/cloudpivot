/**
 * 看板状态流转「回执」处理（从 `TaskBoard.vue#moveTask` 抽出的纯函数层）。
 *
 * # 为什么单独成模块
 *
 * 这段逻辑原本内联在 `moveTask()` 的 `try` 里：请求成功后依次做「卡片状态/进度校正」和
 * 「父任务进度同步」，并且和**请求本身共用一个 `catch`**。于是回执侧任何一次抛错都会走
 * 「请求失败」的**整体回弹**分支：服务端已经 200 落地，页面却把卡片退回源列 ——
 * 这正是 2026-10-30 12:20 流程3 那次「服务端成功、页面未反映」的唯一代码路径。
 *
 * 抽成纯函数（不 import Vue / axios）以后：
 * ① 可以在 `moveTask` 里给它单独的异常边界，保证「回执处理失败 ≠ 回滚」；
 * ② 可以用 `node --test` 直接单测，不需要引入 vitest（见 `frontend/tests/unit/`）。
 *
 * # 数据形状
 *
 * 只取回执里前端真正用到的三个字段（`PATCH /business/task/{id}/status` 的 `data`）：
 * `status` / `progress` / `parent_progress`。后端是 Rust 的 `Vec<ParentProgressRes>`，
 * 正常永远是数组（生产实测顶层任务恒为 `[]`）；但**前端不拿这个假设当契约** ——
 * `null`、缺字段、类型漂移一律按「没有」处理，绝不抛。
 */

/** 卡片上回执会用到的字段（`TaskCard` 的最小投影，便于单测直接用字面量） */
export interface ReceiptCard {
  /** 任务 ID */
  id: string;
  /** 任务状态（大写，如 `DOING`） */
  status?: string;
  /** 进度 0~100 */
  progress?: number;
}

/** 父任务进度重算结果（对应后端 `ParentProgressRes` 的子集） */
export interface ParentProgressLike {
  /** 被重算的任务 ID */
  id?: string;
  /** 重算后的进度 */
  progress?: number;
}

/** 状态流转回执（对应后端 `TaskStatusRes` 的子集） */
export interface StatusReceiptLike {
  /** 落库后的状态 */
  status?: string;
  /** 落库后的进度（状态联动可能抬高） */
  progress?: number;
  /** 被重算的父 / 祖父任务进度 */
  parent_progress?: ParentProgressLike[] | null;
}

/** 回执应用结果（调用方只用来记日志，永远不回滚） */
export interface ReceiptOutcome {
  /** 是否找到了目标卡片并应用回执 */
  applied: boolean;
  /** 同步了几张父任务卡片 */
  parentsSynced: number;
  /** 兜底捕获到的异常（有值只说明回执侧出错，服务端已落地） */
  error?: unknown;
}

/** 是否是合法的有限数字（`NaN` / `Infinity` / 字符串一律不算） */
function isFiniteNumber(value: unknown): value is number {
  return typeof value === 'number' && Number.isFinite(value);
}

/** 是否是「有内容的字符串」 */
function isNonEmptyString(value: unknown): value is string {
  return typeof value === 'string' && value.trim().length > 0;
}

/**
 * 把状态流转回执应用到看板状态上。
 *
 * # 参数
 *
 * - `targetTasks`：目标列的卡片数组（会**原地**修改命中的那张卡）
 * - `taskId`：本次流转的任务 ID
 * - `receipt`：接口回执；`null` / `undefined` 视为「没有回执」，仍把卡片标成 `fallbackStatus`
 * - `fallbackStatus`：回执没带 `status` 时的兜底状态（= 本次拖拽的目标列）
 * - `boardTasks`：板上所有列的卡片（用于父任务进度同步）
 *
 * # 返回值
 *
 * `{ applied, parentsSynced, error }`；函数本身**不抛异常**，出错只落在 `error` 上。
 *
 * # 契约（红线）
 *
 * 调用方拿到 `error` 只许打日志，**不许回滚** —— 能走到这里说明服务端已经确认落地。
 */
export function applyStatusReceipt(args: {
  targetTasks: ReceiptCard[];
  taskId: string;
  receipt: StatusReceiptLike | null | undefined;
  fallbackStatus: string;
  boardTasks?: ReceiptCard[][];
}): ReceiptOutcome {
  const { targetTasks, taskId, receipt, fallbackStatus } = args;
  const boardTasks = Array.isArray(args.boardTasks) ? args.boardTasks : [];

  try {
    // ① 状态：回执优先，缺失用「拖拽目标列」兜底（绝不留空状态把卡片渲染歪）
    const status = isNonEmptyString(receipt?.status) ? receipt.status : fallbackStatus;
    const progress = isFiniteNumber(receipt?.progress) ? receipt.progress : undefined;

    const landed = Array.isArray(targetTasks)
      ? targetTasks.find((item) => item && item.id === taskId)
      : undefined;
    if (landed) {
      landed.status = status;
      // 回执没给进度（或给了非法值）就保留卡片原值，不写 undefined / NaN
      if (progress !== undefined) {
        landed.progress = progress;
      }
    }

    // ② 父任务进度：可能不在本屏，找不到就算了
    const parentsSynced = syncParentProgress(receipt?.parent_progress, boardTasks);

    return { applied: Boolean(landed), parentsSynced };
  } catch (error) {
    // 兜底：回执形状再离谱也不许把异常抛给 moveTask（那会触发整体回滚）
    return { applied: false, parentsSynced: 0, error };
  }
}

/**
 * 父任务进度同步：把回执里的 `{ id, progress }` 刷到板上同 id 的卡片。
 *
 * 父任务可能不在本屏（分页 / 别的列），找不到就跳过；坏元素（缺 id、进度非数）
 * 逐条跳过、不整体丢弃，也**不抛异常**。
 *
 * # 返回值
 *
 * 实际同步了几张卡片。
 */
export function syncParentProgress(
  list: ParentProgressLike[] | null | undefined,
  boardTasks: ReceiptCard[][] | null | undefined,
): number {
  // 后端正常返回数组；null / 缺字段 / 非数组 → 当作「没有要同步的」，直接返回
  if (!Array.isArray(list) || list.length === 0) {
    return 0;
  }
  if (!Array.isArray(boardTasks) || boardTasks.length === 0) {
    return 0;
  }

  let synced = 0;
  for (const progress of list) {
    if (!progress || !isNonEmptyString(progress.id) || !isFiniteNumber(progress.progress)) {
      continue;
    }
    for (const column of boardTasks) {
      if (!Array.isArray(column)) {
        continue;
      }
      const hit = column.find((item) => item && item.id === progress.id);
      if (hit) {
        hit.progress = progress.progress;
        synced += 1;
      }
    }
  }
  return synced;
}
