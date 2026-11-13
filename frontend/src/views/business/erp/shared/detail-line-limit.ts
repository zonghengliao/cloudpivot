/**
 * 单据明细行数上限的**前端口径**（P1-04：明确拒绝 + 可操作提示）。
 *
 * # 与后端的关系
 *
 * 后端 7 个单据各自有 `MAX_LINES = 200`，超限时由
 * `backend/src/business/shared/doc_lines.rs` 的 `too_many_lines()` 生成同一句话。
 * 这里**照抄同一句话**（前端提前提示，后端保存时兜底拒绝）：
 *
 * ```
 * 明细最多 200 行（当前 301 行）。大批量请用「导入向导」：业务 → 通用底座 → 导入向导（导入通道支持 2 万行）
 * ```
 *
 * ⚠ 改文案必须**同时**改后端 `doc_lines.rs`：前端先说、后端再说，两句话不一致
 * 用户会以为是两个问题（同 ATT-08 的教训：文案双份 = 语义双份）。
 *
 * # 为什么只提示、不阻断
 *
 * 卡面决策是「明确拒绝 + 可操作提示」，不是「前端拦死」：明细可能是从来源单
 * 带出来的（采购单待收 201 行），页面上删到 200 行以内的动作本身是合法的。
 * 所以这里只负责「超了就黄字说清楚 + 给个入口」，保存时 400 由后端发。
 */

/** 前端默认上限（与后端 `doc_lines::DEFAULT_MAX_LINES` 同值）。 */
export const DETAIL_MAX_LINES = 200;

/** 导入向导路由名（`frontend/src/router/index.ts` 的 `BusinessImpexpImport`）。 */
export const IMPORT_WIZARD_ROUTE_NAME = 'BusinessImpexpImport';

/** 超限后「该怎么办」的引导文案（与后端 `doc_lines::IMPORT_WIZARD_HINT` 逐字一致）。 */
export function importWizardHint(): string {
  return '大批量请用「导入向导」：业务 → 通用底座 → 导入向导（导入通道支持 2 万行）';
}

/** 是否超出行数上限（`>`，恰好等于上限算合法）。 */
export function overDetailLineLimit(count: number, limit: number = DETAIL_MAX_LINES): boolean {
  return count > limit;
}

/** 超限提示全文（与后端 400 的 `message` 同口径）。 */
export function detailLineLimitMessage(count: number, limit: number = DETAIL_MAX_LINES): string {
  return `明细最多 ${limit} 行（当前 ${count} 行）。${importWizardHint()}`;
}
