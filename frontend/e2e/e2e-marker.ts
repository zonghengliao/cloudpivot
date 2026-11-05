/**
 * e2e 夹具标记（marker）—— 前端侧
 * =================================
 *
 * 「这一行是不是 e2e 造的？」这件事必须有**一个可机读的答案**，否则一旦有人把
 * `E2E_BASE_URL` 指到生产，造出来的任务/公告/通知和真实业务行长**一模一样**，
 * 事后只能靠人眼猜前缀（2026-10-30/31 生产库就这么混进了 9 个活跃 E2E 任务 +
 * 105 条通知 + 1 条已发布公告 + 221 条评论，见卡 `t_44766ab3`）。
 *
 * 本文件与 `scripts/verify/e2e_marker.py` 是**同一个约定的两半**：
 *
 *   | 对象   | 标记                      | 判据（python 侧，唯一实现）        |
 *   |--------|---------------------------|-----------------------------------|
 *   | 任务   | 标题前缀 `[E2E]`          | `biz_task.title like '[E2E]%'`     |
 *   | 仓库   | 编码前缀 `E2E-`           | `biz_warehouse.warehouse_code`     |
 *   | 上传件 | 文件名前缀 `e2e` / `e9-`  | `sys_file.original_name`           |
 *   | 导出   | 导出名前缀 `e2e`          | `sys_export_task.export_name`      |
 *
 * ⚠️ 改前缀必须**两边一起改**：`python3 scripts/verify/e2e_marker.py --self-test`
 * 会读本文件里的 `E2E_MARK` 与 python 侧常量比对，不一致直接失败。
 *
 * 强制点：`support.ts::newApi().createTask()` 一律经 [`markTitle`] 过一遍
 * （幂等：已经带标记的不重复加）。也就是说**用例忘了写前缀也跑不出未标记的任务**
 * —— 这是 2026-10-31 那批残留的直接根因修复。
 */

/** 任务标题标记（与 `scripts/verify/e2e_marker.py::TASK_TITLE_PREFIX` 必须一致） */
export const E2E_MARK = '[E2E]';

/** 仓库/库区编码前缀（`biz_warehouse.warehouse_code`） */
export const E2E_WAREHOUSE_PREFIX = 'E2E-';

/**
 * 给标题套标记（幂等）。
 *
 * 传进来的标题**已经**以 `[E2E]` 开头就原样返回 —— 各 spec 手写的
 * `[E2E]拖拽-正常-${RUN}` 与「完全没写前缀」的标题走同一条路，不会出现
 * `[E2E][E2E]…` 这种双前缀。
 */
export function markTitle(title: string): string {
  const t = (title ?? '').trim();
  if (!t) return E2E_MARK; // 空标题也要能定位（后端会拒，但至少不带污染）
  return t.startsWith(E2E_MARK) ? t : `${E2E_MARK}${t}`;
}

/** 仓库编码/名称套标记（幂等，口径同 `markTitle`） */
export function markWarehouse(code: string): string {
  const c = (code ?? '').trim();
  if (!c) return `${E2E_WAREHOUSE_PREFIX}EMPTY`;
  return c.startsWith(E2E_WAREHOUSE_PREFIX) ? c : `${E2E_WAREHOUSE_PREFIX}${c}`;
}

/** 上传件/导出件名套标记（幂等）：`e2e-<原文件名>` */
export function markFileName(name: string): string {
  const n = (name ?? '').trim();
  if (!n) return 'e2e-empty.txt';
  return /^e2e/i.test(n) ? n : `e2e-${n}`;
}

/** 是否带标记（断言用；`expect(marked, '…').toBe(true)` 里给失败文案） */
export function isMarkedTitle(title: string): boolean {
  return (title ?? '').startsWith(E2E_MARK);
}
