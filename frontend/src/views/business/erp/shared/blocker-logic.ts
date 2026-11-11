/**
 * 挡路单据的**展示逻辑**（PERIOD-CLOSE / YEAR-END 共用）—— 不 import vue，可 `tsx --test` 单测。
 *
 * # 为什么单独一个文件
 *
 * 这些函数回答的是同一组问题，与「会计期间」模块本身无关：
 *
 * ```text
 * 这是哪种单据？        blockingDocTypeLabel
 * 该谁处理？            ownerRoleLabel / ownerText
 * 金额多少、等了多久？  blockingMoneyText / waitingText
 * 能不能催？            canRemindItem / remindableItems
 * 催完提示什么？        remindToast
 * ```
 *
 * 年结检查清单（`year-end`）与结账弹窗（`period`）用**同一套**口径，
 * 所以放在 `shared/` 下；`period/period-logic.ts` 原样 `export *` 再导出，
 * 老 import 路径（含单测）保持不变。
 */

import type { PeriodBlockingItem, PeriodRemindResult } from '@/api/business/erp';

/**
 * 单据类型 → 中文（与后端 `period_items::doc_type_label` 同口径）。
 *
 * 未知类型原样返回：后端加了新单据类型时页面不该显示空白。
 */
const BLOCKING_DOC_TYPE_LABELS: Record<string, string> = {
  VOUCHER: '凭证',
  STOCK_IN: '收货',
  PURCHASE_ORDER: '采购',
  STOCK_TRANSFER: '调拨',
  INVENTORY: '库存',
  // 成本核算批次（ARCH-9 §4.11 第 5 项 blocker）—— 与后端 `doc_type_label` 同口径
  COST_PERIOD: '成本批次',
};

/** 单据类型中文（未知类型原样返回） */
export function blockingDocTypeLabel(docType: string | null | undefined): string {
  const key = (docType ?? '').trim();
  if (!key) {
    return '—';
  }
  return BLOCKING_DOC_TYPE_LABELS[key] ?? key;
}

/**
 * 角色码 → 中文岗位名。
 *
 * 只是**展示**用：后端给的是角色码（`ACCOUNTANT`），催办也按角色扇出，
 * 所以这里查不到就原样显示角色码（不猜、不吞）。
 */
const OWNER_ROLE_LABELS: Record<string, string> = {
  SUPER_ADMIN: '超级管理员',
  ADMIN: '管理员',
  FINANCE: '财务',
  FINANCE_MANAGER: '财务经理',
  FINANCE_DIRECTOR: '财务总监',
  ACCOUNTANT: '会计',
  WAREHOUSE: '仓管',
  WAREHOUSE_MANAGER: '仓管',
  PURCHASER: '采购',
  PURCHASE_MANAGER: '采购经理',
  GENERAL_MANAGER: '总经理',
  DEPT_LEADER: '部门主管',
};

/** 角色码 → 中文（查不到原样返回） */
export function ownerRoleLabel(role: string | null | undefined): string {
  const key = (role ?? '').trim();
  if (!key) {
    return '待分配';
  }
  return OWNER_ROLE_LABELS[key] ?? key;
}

/**
 * 「该谁处理」一栏的文案：`会计(李四)` / `会计` / `待分配`。
 *
 * - 有具体人 → 角色(姓名)：能直接点名催；
 * - 只有角色 → 只写角色：该角色全体都会收到催办；
 * - 都没有 → 「待分配」：**不瞎猜**，催办按钮置灰。
 */
export function ownerText(item: {
  owner_role?: string | null;
  owner_name?: string | null;
}): string {
  const role = (item.owner_role ?? '').trim();
  const name = (item.owner_name ?? '').trim();
  if (!role) {
    return '待分配';
  }
  const label = ownerRoleLabel(role);
  return name ? `${label}(${name})` : label;
}

/** 这一行能不能催办（有角色或有具体人；都没有就是「待分配」） */
export function canRemindItem(item: {
  owner_role?: string | null;
  owner_name?: string | null;
}): boolean {
  return Boolean((item.owner_role ?? '').trim() || (item.owner_name ?? '').trim());
}

/** 金额文案：`¥1,234.00`；空值 → `—`（不是所有单据都有金额） */
export function blockingMoneyText(amount: string | number | null | undefined): string {
  if (amount === null || amount === undefined || amount === '') {
    return '—';
  }
  const value = typeof amount === 'number' ? amount : Number(amount);
  if (!Number.isFinite(value)) {
    return String(amount);
  }
  return `¥${value.toLocaleString('zh-CN', {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  })}`;
}

/**
 * 「已等」文案：`3 天` / `5 小时` / `不到 1 小时` / `—`。
 *
 * 后端给的是 `waiting_since`（单据创建时间），按**自然时长**算，不做工作日折算
 * （财务只关心「压了几天」，折算工作日反而要解释节假日表）。
 */
export function waitingText(
  since: string | null | undefined,
  now: Date = new Date(),
): string {
  const text = (since ?? '').trim();
  if (!text) {
    return '—';
  }
  const start = new Date(text);
  if (Number.isNaN(start.getTime())) {
    return '—';
  }
  const hours = (now.getTime() - start.getTime()) / 3_600_000;
  if (hours < 0) {
    return '—';
  }
  if (hours < 1) {
    return '不到 1 小时';
  }
  if (hours < 24) {
    return `${Math.floor(hours)} 小时`;
  }
  return `${Math.floor(hours / 24)} 天`;
}

/** 这一类 blocker 有没有单据级明细（没有 → 退回「一句话 + 去看看」老样式） */
export function blockerHasItems(blocker: { items?: unknown[] }): boolean {
  return Array.isArray(blocker.items) && blocker.items.length > 0;
}

/** 明细里**可催办**的那些（有处理人） */
export function remindableItems<T extends { owner_role?: string | null; owner_name?: string | null }>(
  items: readonly T[],
): T[] {
  return items.filter((item) => canRemindItem(item));
}

/** 明细总数（弹窗标题用：「共 N 张挡路单据」） */
export function blockingItemCount(
  blockers: ReadonlyArray<{ items?: ReadonlyArray<unknown> }>,
): number {
  return blockers.reduce((sum, blocker) => sum + (blocker.items?.length ?? 0), 0);
}

/**
 * 催办结果 → toast 文案。
 *
 * 后端已经给了一句人话（`message`），优先用它；缺失时按计数合成，
 * 保证「点了催办」一定有个明确结论，而不是静默成功。
 */
export function remindToast(result: {
  sent: number;
  failed: number;
  skipped_no_owner: number;
  skipped_recent: number;
  total: number;
  message?: string;
}): string {
  const message = (result.message ?? '').trim();
  if (message) {
    return message;
  }
  if (result.total === 0) {
    return '没有可催办的单据';
  }
  const parts = [`已催办 ${result.sent} 张`];
  if (result.skipped_recent > 0) {
    parts.push(`${result.skipped_recent} 张 24 小时内已催过`);
  }
  if (result.skipped_no_owner > 0) {
    parts.push(`${result.skipped_no_owner} 张未指定处理人`);
  }
  if (result.failed > 0) {
    parts.push(`${result.failed} 张发送失败`);
  }
  return parts.join('，');
}

/**
 * 面板要的最小阻塞项形状（期间 `Blocker` / 年结 `YearEndBlocker` 都满足）。
 *
 * 用**结构化类型**而不是直接 import `PeriodCloseBlocker | YearEndBlocker`：
 * 两个后端的 `type` 都是字符串联合，硬绑死会让面板在多一个 blocker 类型时编译不过。
 */
export interface BlockingBlocker {
  /** 阻塞项类型（`DRAFT_VOUCHER` / `DOC_UNFINISHED` / `INVARIANT` / `DOC_UNFINISHED`…） */
  type: string;
  /** 全量条数（与 `items.length` 不等 = 被截断到 50 条） */
  count?: number;
  /** 一句话摘要（后端已点名数量） */
  message: string;
  /** 去列表处理 */
  link?: string;
  /** 明细（空 / 缺省 = 这一类没有单据级明细） */
  items?: PeriodBlockingItem[];
}

/**
 * 催办函数签名（期间催办 / 年结催办各自的接口封装都符合它）。
 *
 * 面板**不自己发请求**：调哪个接口、带什么额外参数（期间 id / 年度）由调用方决定，
 * 面板只负责「催哪一类 + 哪几张」。
 */
export type BlockerRemindFn = (payload: {
  blocker_kind: string | null;
  item_ids: string[];
}) => Promise<PeriodRemindResult>;
