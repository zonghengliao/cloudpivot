/**
 * 凭证「审核 / 反审核 / 记账」三态流转的**纯逻辑**（P0-FIX-4B）。
 *
 * # 为什么单独一个文件
 *
 * `voucher-logic.ts` 已经 400 行（状态色 / 借贷平衡 / 筛选 / 红冲审批），
 * 而「哪个状态能点哪个按钮、没权限为什么不让点」恰恰是**财务内控**本身：
 *
 * ```text
 * DRAFT    → 审核     （DRAFT → AUDITED，PRD E4-09）
 * AUDITED  → 反审核   （AUDITED → DRAFT，必填原因，PRD E4-09）
 * AUDITED  → 记账     （AUDITED → POSTED，PRD E4-10；记账后不可反审核）
 * POSTED   → 无操作   （只能红冲，更正靠新开红字凭证）
 * CANCELLED→ 无操作
 * ```
 *
 * 这一层是 `.vue` 里 `v-if` 判据的**唯一真相**：按钮显不显示、置不置灰、
 * 置灰时说什么，全部由这里返回，页面只负责渲染。这样 `pnpm test:unit`
 * 能直接打状态矩阵（没有 DOM 也能验），改错一行立刻红。
 *
 * # 与后端的分工（不重复校验，但要「先说清楚」）
 *
 * 真正的闸在后端（状态机 `shared/state_machine.rs` + 4A 的 handler）：
 * 非法流转 409、制单人自审 403、反审核无原因 400。
 * 前端这一层的职责是**别让人点了才看见红字**：状态不对的按钮直接不渲染，
 * 权限不够 / 自审的直接置灰并写明原因。
 *
 * ⚠️ 后端 P0-FIX-4A（DRAFT/AUDITED 两态 + 三个接口）尚未合并进 main 时，
 * 列表里全是 `POSTED` → 这三个按钮一个都不出现（这是**正确**的降级：
 * 后端没有的能力不摆假入口）。接口契约见 `voucher-audit-api.ts` 文件头。
 */

import type { VoucherStatus } from '@/api/business/erp';

/** 审核链上的三个动作（顺序即界面顺序）。 */
export type VoucherAuditAction = 'audit' | 'un-audit' | 'post';

/** 一个动作的静态声明（文案 / 主题 / 落点状态 / 权限码）。 */
export interface VoucherActionSpec {
  /** 动作 key（与接口一一对应） */
  key: VoucherAuditAction;
  /** 按钮文案 */
  label: string;
  /** TDesign 主题 */
  theme: 'primary' | 'warning' | 'success';
  /** 这个动作把凭证推到哪个状态 */
  to: VoucherStatus;
  /** 是否必须填原因（只有反审核） */
  requiresReason: boolean;
  /** 权限码（与后端 `#[alun::permission]` 声明同名） */
  permission: string;
}

/** 动作声明表（文案与权限码的唯一真相）。 */
export const VOUCHER_ACTION_SPECS: Readonly<Record<VoucherAuditAction, VoucherActionSpec>> = {
  audit: {
    key: 'audit',
    label: '审核',
    theme: 'primary',
    to: 'AUDITED',
    requiresReason: false,
    permission: 'business:erp:voucher:audit',
  },
  'un-audit': {
    key: 'un-audit',
    label: '反审核',
    theme: 'warning',
    to: 'DRAFT',
    requiresReason: true,
    permission: 'business:erp:voucher:un-audit',
  },
  post: {
    key: 'post',
    label: '记账',
    theme: 'success',
    to: 'POSTED',
    requiresReason: false,
    permission: 'business:erp:voucher:post',
  },
};

/**
 * 当前状态**允许**的动作（按界面显示顺序）。
 *
 * 与 PRD E4-09 / E4-10 逐字对应：
 * - 草稿只有「审核」（还没有人复核过，不能直接记账）；
 * - 已审核才有「反审核」+「记账」（记账后不可反审核 → POSTED 无操作）；
 * - 已记账 / 已作废什么都没有（更正走红冲，不原地改状态）。
 */
export function allowedVoucherActions(status: VoucherStatus | string | null | undefined): VoucherAuditAction[] {
  switch (status) {
    case 'DRAFT':
      return ['audit'];
    case 'AUDITED':
      return ['un-audit', 'post'];
    default:
      return [];
  }
}

/** 界面上一个按钮的最终形态（状态已筛过，这里只剩「能不能点 + 不能点的原因」）。 */
export interface VoucherActionButton extends VoucherActionSpec {
  /** 是否置灰 */
  disabled: boolean;
  /** 置灰原因（可点 = 空串） */
  reason: string;
}

/** 判定按钮可用性需要的上下文。 */
export interface VoucherActionContext {
  /** 当前账号是否持有某权限码（`useUserStore().hasPermission`） */
  hasPermission: (code: string) => boolean;
  /** 当前登录用户 id（不知道就传 null，不做自审判定） */
  currentUserId?: string | null;
  /** 凭证制单人 id（后端未下发该字段时传 null，不做自审判定） */
  createdBy?: string | null;
}

/** 制单人自审的置灰文案（PRD / 4A：制单 ≠ 审核）。 */
export const SELF_AUDIT_BLOCKED_REASON =
  '制单人不能审核自己录入的凭证（内控：制单与复核必须分离），请换有审核权限的同事操作';

/** 没权限的置灰文案（点名权限码，方便找管理员开）。 */
export function permissionBlockedReason(code: string): string {
  return `当前账号没有「${code}」权限，请联系系统管理员在「角色权限」里授予`;
}

/**
 * 状态 + 权限 + 自审 → 界面按钮清单。
 *
 * 返回的数组**已经按状态筛过**（草稿只会有「审核」），
 * 每个按钮再带 `disabled` / `reason`。
 */
export function voucherActionButtons(
  status: VoucherStatus | string | null | undefined,
  context: VoucherActionContext,
): VoucherActionButton[] {
  const actions = allowedVoucherActions(status);
  return actions.map((key) => {
    const spec = VOUCHER_ACTION_SPECS[key];
    if (!context.hasPermission(spec.permission)) {
      return { ...spec, disabled: true, reason: permissionBlockedReason(spec.permission) };
    }
    if (key === 'audit' && isSelfAudit(context)) {
      return { ...spec, disabled: true, reason: SELF_AUDIT_BLOCKED_REASON };
    }
    return { ...spec, disabled: false, reason: '' };
  });
}

/** 是不是「自己审自己」（两个 id 都未知时一律 false：宁可不拦，也不误拦）。 */
export function isSelfAudit(context: Pick<VoucherActionContext, 'currentUserId' | 'createdBy'>): boolean {
  const me = (context.currentUserId ?? '').trim();
  const creator = (context.createdBy ?? '').trim();
  return Boolean(me && creator && me === creator);
}

/** 反审核原因的最小长度（只要求非空白；后端 4A 可能更严，超了它会 400 并点名）。 */
export const UNAUDIT_REASON_MIN_LENGTH = 1;

/**
 * 反审核原因校验：必填。
 *
 * @returns 错误文案（空串 = 通过）
 */
export function unAuditReasonError(reason: string | null | undefined): string {
  const text = (reason ?? '').trim();
  if (text.length < UNAUDIT_REASON_MIN_LENGTH) {
    return '反审核原因必填（会写进凭证状态日志，审计要看「为什么把已审核打回草稿」）';
  }
  return '';
}

/** 动作成功后的一句话（记账与审核后果不同，文案不能混）。 */
export function voucherActionDoneText(action: VoucherAuditAction, voucherNo: string): string {
  switch (action) {
    case 'audit':
      return `已审核：${voucherNo} 进入「已审核」，可以记账`;
    case 'un-audit':
      return `已反审核：${voucherNo} 回到「草稿」，改完可重新提交审核`;
    case 'post':
      return `已记账：${voucherNo} 进入总账（记账后不可反审核，更正请红冲）`;
    default:
      return voucherNo;
  }
}

/** 动作前的二次确认文案（记账动账，必须说清后果）。 */
export function voucherActionConfirmText(action: VoucherAuditAction, voucherNo: string): string {
  switch (action) {
    case 'audit':
      return `审核 ${voucherNo}：确认分录科目与金额无误？审核后状态变「已审核」，下一步才能记账。`;
    case 'un-audit':
      return `把 ${voucherNo} 反审核回「草稿」：状态日志会记录原因，改完需要重新审核。`;
    case 'post':
      return `记账 ${voucherNo}：记账后这张凭证进入总账 / 三大报表，且**不可再反审核**（更正只能红冲开新凭证）。`;
    default:
      return voucherNo;
  }
}

/**
 * 详情抽屉里「审核状态」那一句说明（按钮区下方）。
 *
 * 为什么要写：`POSTED` / `CANCELLED` 状态下一个审核按钮都没有，用户会以为
 * 「按钮没渲染出来」。这里把「为什么没有」写成人话，并给出下一步该走哪。
 */
export function voucherAuditNote(status: VoucherStatus | string | null | undefined): string {
  switch (status) {
    case 'DRAFT':
      return '草稿：还没复核。点「审核」后进入「已审核」，此时凭证仍不进总账。';
    case 'AUDITED':
      return '已审核：可以「记账」进总账，或「反审核」打回草稿重改（反审核要填原因）。';
    case 'POSTED':
      return '已记账：已进总账 / 报表，审核与反审核都不可用。更正请用「红冲」开一张红字凭证。';
    case 'CANCELLED':
      return '已作废：这张凭证不会再进账，没有可执行的审核动作。';
    default:
      return '';
  }
}

/**
 * 审批进度该看哪个实例（P1-FIX-11）。
 *
 * 凭证的审批实例只有**红冲审批**（`pending_reverse_instance_id`，P-FLOW-3B）。
 * 后端 4A 若给审核链也挂实例（`approval_instance_id`），这里优先用它 —— 两个字段
 * 都可能是 `null`/不存在，取先有值的那个；都没有 → `null`（进度块整块不渲染）。
 *
 * 判据放在这里而不是 `.vue` 里，是为了单测能直接打「两个字段任意组合」的矩阵。
 */
export function voucherApprovalInstanceId(
  row:
    | {
        pending_reverse_instance_id?: string | null;
        approval_instance_id?: string | null;
      }
    | null
    | undefined,
): string | null {
  if (!row) {
    return null;
  }
  const pending = (row.pending_reverse_instance_id ?? '').trim();
  if (pending) {
    return pending;
  }
  const audit = (row.approval_instance_id ?? '').trim();
  return audit || null;
}

