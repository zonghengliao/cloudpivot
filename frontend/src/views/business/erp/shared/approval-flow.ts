/**
 * 「送审 / 撤回」的反馈文案与结果判据（P-FLOW-3B）。
 *
 * # 为什么单独一个纯逻辑文件
 *
 * 应付作废、凭证红冲这两条链从 P-FLOW-3B 起**都走审批**，同一份出参形状：
 *
 * ```json
 * { "outcome": "PENDING_APPROVAL", "message": "…", "approval_instance_id": "…", …单据字段平铺… }
 * ```
 *
 * 于是最容易写错的地方不是渲染，而是**提示文案与「账到底动没动」的对应关系**：
 *
 * | outcome | 真实后果 | 允许的提示 | 绝对不许说 |
 * |---|---|---|---|
 * | `PENDING_APPROVAL` | 只发起了审批，**账一分没动** | 「已提交审批，等待财务经理复核」+ 刷新列表 | 「已作废」「已红冲」「付款成功」 |
 * | `CANCELLED` | 未配流程 → 降级**直接作废** | 「已作废：AP-…」 | —— |
 * | `REVERSED` | 未配流程 → 降级**直接红冲** | 「已红冲：生成红字凭证 PZR-…」 | —— |
 *
 * 这个对应关系写错，用户会以为钱已经退了 / 账已经冲了 —— 财务页最不能犯的一类错。
 * 所以判据全部搬到这里（`.vue` 里只 `MessagePlugin[feedback.level](feedback.text)`），
 * 单测直接打 `submitFeedback()`，改一行 `if` 就会被用例钉住（与 D7 的
 * `utils/batch-action.ts` 同一套理由：把 `v-if` 里的判据抽出来才能测）。
 *
 * # 文案口径（逐字，验收会 grep）
 *
 * - 送审中：**「已提交审批，等待财务经理复核」**（卡面 §三 ③ 的原话，两个单据共用）；
 * - 作废直接生效：`已作废：<单号>`（保留 P7 老文案）；
 * - 红冲直接生效：`已红冲：生成红字凭证 <红字凭证号>`（保留 P7 老文案）；
 * - 撤回：`已撤回作废审批：<单号> 回到「未付」` / `已撤回红冲审批：<单号> 仍是「已记账」`。
 */

/** 送审后「审批中」的 outcome（后端 `payable_payment.rs::OUTCOME_CANCEL_PENDING` / `voucher_write.rs::OUTCOME_REVERSE_PENDING`）。 */
export const OUTCOME_PENDING_APPROVAL = 'PENDING_APPROVAL'

/** 应付作废「未配流程 → 直接作废」的 outcome（`OUTCOME_CANCELLED`）。 */
export const OUTCOME_CANCELLED = 'CANCELLED'

/** 凭证红冲「未配流程 → 直接红冲」的 outcome（`OUTCOME_REVERSED`）。 */
export const OUTCOME_REVERSED = 'REVERSED'

/**
 * 送审中的统一提示（卡面 §三 ③ 原话）。
 *
 * 两个单据共用一句话：审批人都是财务经理（凭证金额 > 1 万时连带财务总监会签，
 * 但「等财务经理复核」对两种情形都成立，不必在前端分叉 —— 分叉错了反而更误导）。
 */
export const PENDING_APPROVAL_HINT = '已提交审批，等待财务经理复核'

/** 走审批链的两类单据。 */
export type FlowDocKind = 'payable-cancel' | 'voucher-reverse'

/** 出参里本文件用得到的最小形状（后端两个 `*SubmitRes` 都有这三列）。 */
export interface SubmitResultLike {
  /** `PENDING_APPROVAL` / `CANCELLED` / `REVERSED`（后端新增取值时按 `unknown` 兜） */
  outcome?: string | null
  /** 后端给的一句话（未知 outcome 的兜底文案用它） */
  message?: string | null
  /** 审批实例 id（未配流程 / 直接生效时为 null） */
  approval_instance_id?: string | null
}

/** 提示级别（直接对应 `MessagePlugin.info` / `.success` / `.error`）。 */
export type FeedbackLevel = 'info' | 'success' | 'error'

/** 一条要弹给用户的提示。 */
export interface Feedback {
  /** 级别：审批中 = info（不是成功，账没动）；直接生效 = success */
  level: FeedbackLevel
  /** 文案 */
  text: string
}

/**
 * 这次提交是不是**只是送审**（账没动）。
 *
 * 判定只认 `outcome === 'PENDING_APPROVAL'`：
 * 不能用「有没有 `approval_instance_id`」推断 —— 降级直做的分支该字段也是 null，
 * 但反过来（将来后端给出了实例 id 却没改 outcome）就判错了。
 */
export function isPendingApproval(result: SubmitResultLike | null | undefined): boolean {
  return (result?.outcome ?? '') === OUTCOME_PENDING_APPROVAL
}

/** 「审批中」徽标文案（列表 / 详情共用；与后端 `status_label` / 需求原文一致）。 */
export function pendingBadgeLabel(kind: FlowDocKind): string {
  return kind === 'payable-cancel' ? '作废审批中' : '红冲审批中'
}

/** 单据类型的口语名（拼进提示里：「应付单 AP-…」/「凭证 PZ-…」）。 */
function docNoun(kind: FlowDocKind): string {
  return kind === 'payable-cancel' ? '应付单' : '凭证'
}

/**
 * 送审后的提示。
 *
 * - `PENDING_APPROVAL` → **info**，「已提交审批，等待财务经理复核（单号）：<接下来会发生什么>」；
 * - `CANCELLED`（应付）→ **success**，「已作废：单号」；
 * - `REVERSED`（凭证）→ **success**，「已红冲：生成红字凭证 红字号」；
 * - 其它（后端将来加新取值）→ **info**，用后端 `message` 原样兜底，不吞掉。
 *
 * `redNo` 只有凭证红冲用得到（降级直做时后端把红字凭证号放在 detail 里）。
 */
export function submitFeedback(
  kind: FlowDocKind,
  result: SubmitResultLike | null | undefined,
  docNo: string,
  redNo?: string | null,
): Feedback {
  const no = (docNo ?? '').trim() || '（无单号）';
  if (isPendingApproval(result)) {
    const tail =
      kind === 'payable-cancel'
        ? `：${docNoun(kind)}仍是「未付」，审批通过才会真正作废`
        : `：${docNoun(kind)}仍是「已记账」，审批通过才生成红字凭证`;
    return { level: 'info', text: `${PENDING_APPROVAL_HINT}（${no}）${tail}` };
  }
  const outcome = (result?.outcome ?? '').trim();
  if (kind === 'payable-cancel' && outcome === OUTCOME_CANCELLED) {
    return { level: 'success', text: `已作废：${no}` };
  }
  if (kind === 'voucher-reverse' && outcome === OUTCOME_REVERSED) {
    const red = (redNo ?? '').trim();
    return { level: 'success', text: red ? `已红冲：生成红字凭证 ${red}` : `已红冲：${no}` };
  }
  const message = (result?.message ?? '').trim();
  return { level: 'info', text: message || `提交完成（outcome=${outcome || '未返回'}）` };
}

/**
 * 撤回后的提示。
 *
 * 撤回**不改账**：应付单回 `OPEN`（未付）、凭证一直是 `POSTED`（已记账），
 * 所以文案必须把「回到了什么状态」说清楚，别让人以为撤回把单据也删了。
 */
export function withdrawFeedback(kind: FlowDocKind, docNo: string, statusLabel: string): Feedback {
  const no = (docNo ?? '').trim() || '（无单号）';
  const label = (statusLabel ?? '').trim() || (kind === 'payable-cancel' ? '未付' : '已记账');
  return kind === 'payable-cancel'
    ? { level: 'success', text: `已撤回作废审批：${no} 回到「${label}」` }
    : { level: 'success', text: `已撤回红冲审批：${no} 仍是「${label}」` };
}
