/**
 * 流程任务详情页的**唯一主按钮**（P-FLOW-1C · 设计 §4.5 Q4 的「UI 上的三条硬要求」第 1 条）。
 *
 * ```text
 * on_complete=DIRECT   → [ 完成 ]
 * on_complete=VERIFY   → [ 提交验收 ]
 * on_complete=APPROVE  → [ 完成 ]  （副文案：「完成后需审批」）
 * need_review=true 且当前是验收人 → [ 验收通过 ] [ 打回 ]
 * ```
 *
 * # 为什么独立成 `.ts`
 *
 * `StatusActions.vue` 原来在 `DOING` 态会同时渲染「提交验收」（白名单边 `DOING→REVIEW`）
 * 和「直接完成」（`DOING→DONE` + `need_review=false` 才放行）。对普通任务是**合理**的：
 * 用户自己决定要不要送审。
 *
 * 但**流程任务的「完成方式」是节点配置说了算**，不是干活的人临时决定 —— 面板上选了
 * `VERIFY` 就该只有「提交验收」，选了 `DIRECT` 就该只有「完成」。
 * 这条判据放进 `.ts` 才能单测（4 种 `on_complete × need_review` 组合各一条断言），
 * 塞在组件里只能靠人手点。
 *
 * # 与 R18 的关系
 *
 * `APPROVE + need_review=true` 不可能出现（R18 在发布期就拒，面板上也互斥），
 * 这里仍然按「先看 need_review」处理，属于防守性写法。
 */

/** 流程上下文里本卡要用的两件事（就是 `TaskFlowDetail` 的子集） */
export interface FlowCompletion {
  /** 节点配置 `config.task.on_complete`：`DIRECT` / `VERIFY` / `APPROVE` */
  on_complete: string;
  /** 节点配置 `config.task.need_review` */
  need_review: boolean;
}

/** 主按钮的稳定标识（与 `StatusActions.vue` 里 `TransitionRule.key` 同名） */
export type FlowMainKey = 'submit-review' | 'direct-done';

/**
 * 取流程任务在 `DOING` 态该出现的那**一个**主按钮。
 *
 * - `need_review = true` → `submit-review`（提交验收）；
 * - 否则 → `direct-done`（完成）。
 *
 * `on_complete=VERIFY` 必然 `need_review=true`（R18），所以两种判据在这里同序，
 * 不需要额外分支 —— 也就不用担心它们打架。
 */
export function flowMainKey(completion: FlowCompletion | null | undefined): FlowMainKey {
  if (completion?.need_review) {
    return 'submit-review';
  }
  return 'direct-done';
}

/** 主按钮文案（术语锁定：验收 = 「提交验收」，不是「提交审核」） */
export function flowMainLabel(completion: FlowCompletion | null | undefined): string {
  return flowMainKey(completion) === 'submit-review' ? '提交验收' : '完成';
}

/**
 * 主按钮下方的副文案（`null` = 不显示）。
 *
 * 只有 `on_complete=APPROVE` 需要说明「点完还得等审批」——
 * 否则用户点「完成」后看到节点仍「处理中」会以为没生效。
 */
export function flowMainNote(completion: FlowCompletion | null | undefined): string | null {
  if ((completion?.on_complete ?? '').trim().toUpperCase() === 'APPROVE') {
    return '完成后需审批';
  }
  return null;
}

/** 该 key 是否属于「完成类」按钮（DOING 态要做唯一化的那几个） */
export function isFlowCompleteKey(key: string): boolean {
  return key === 'submit-review' || key === 'direct-done';
}

/**
 * 流程详情接口 → 主按钮用的完成方式（P-FLOW-1C 补漏，后端 `138d39a`）。
 *
 * # 为什么单独成一个函数
 *
 * `GET /business/task/{id}/flow` 从 1C 补漏起**恒下发** `on_complete`，值域
 * `string | null`（没配置该项就是 `null`）。这段「`null` 时按 `need_review` 推断」
 * 的规则原先内联在 `detail.vue` 的 `computed` 里，`APPROVE` 被抹成 `DIRECT`
 * 的回归（副文案「完成后需审批」不显示）只能在真机上看出来。
 * 抽出来之后这条回归有单测兜（`tests/unit/task-flow-approve-note.test.ts`）。
 *
 * # 规则
 *
 * - `flow = null`（手工任务 / 还没拉到）→ `null`，整块流程 UI 不渲染；
 * - `on_complete` 有值 → **原样透传**（`APPROVE` 必须透传，否则副文案消失）；
 * - `on_complete = null`（节点没配完成方式）→ 按 `need_review` 推断：
 *   `true` → `VERIFY`，否则 `DIRECT`（与真实配置一致，不会显示错按钮）。
 */
export function flowCompletionFrom(
  flow: { on_complete: string | null } | null | undefined,
  needReview: boolean,
): FlowCompletion | null {
  if (!flow) {
    return null;
  }
  return {
    on_complete: flow.on_complete ?? (needReview ? 'VERIFY' : 'DIRECT'),
    need_review: needReview,
  };
}

/**
 * 把一组流转按钮收敛成「流程任务的按钮组」。
 *
 * # 规则（只动 `DOING` 态）
 *
 * 1. 完成类的两个（`submit-review` / `direct-done`）只留 {@link flowMainKey} 那一个；
 * 2. 留的那个按钮**改文案**为 {@link flowMainLabel}；
 * 3. 其它按钮（取消 / 重开 / 验收通过 / 打回 / 恢复）**原样保留** ——
 *    卡面说的是「只出现 1 个**主**按钮」，不是「只有 1 个按钮」。
 *
 * # 参数
 * - `actions`：原本要渲染的按钮（已过白名单 + 角色位 + need_review 过滤）
 * - `completion`：流程上下文（`non-null` = 这张任务确实由流程生成）
 */
export function applyFlowMainButton<T extends { key: string; label: string }>(
  actions: T[],
  completion: FlowCompletion | null | undefined,
): T[] {
  if (!completion) {
    return actions;
  }
  const completes = actions.filter((action) => isFlowCompleteKey(action.key));
  if (completes.length <= 1) {
    // 只有一个（或没有）：仍然统一文案（`need_review=false` 时 `direct-done` 的默认文案是「直接完成」）
    return actions.map((action) =>
      isFlowCompleteKey(action.key) ? { ...action, label: flowMainLabel(completion) } : action,
    );
  }
  const mainKey = flowMainKey(completion);
  return actions
    .filter((action) => !isFlowCompleteKey(action.key) || action.key === mainKey)
    .map((action) => (isFlowCompleteKey(action.key) ? { ...action, label: flowMainLabel(completion) } : action));
}
