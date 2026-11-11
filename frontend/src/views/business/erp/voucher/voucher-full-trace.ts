/**
 * 凭证详情「全链路」标签页的数据模型（单据 → 审批 → 凭证 → 分录）—— VTRACE-5。
 *
 * # 这一层解决什么
 *
 * 四段的数据早就都在库里，缺的只是「把它们摆成一条能读的时间轴」：
 *
 * | 段 | 数据来源 | 已有能力 |
 * |---|---|---|
 * | ① 来源单据 | 凭证详情 `source_type / source_id / source_no / source_doc_no` | VTRACE-1 路由表 + VTRACE-2 `voucher-source-link.ts` |
 * | ② 审批记录 | `GET /business/flow/instance/by-biz`（本卡新增）+ 上游单据反射 | 流程引擎的 `biz_flow_task` 时间线 |
 * | ③ 本凭证 | 凭证详情（号 / 日期 / 状态 / 记账时间 / 合计） | 详情接口 |
 * | ④ 分录明细 | 凭证详情 `entries[]` | 详情接口 |
 *
 * 判断全部放在这里（纯函数，`tests/unit/vtrace-voucher-full-trace.test.ts` 逐条钉死），
 * `.vue` 只负责渲染 + 取数。理由是这一屏最容易犯的错是**语义错**而不是渲染错：
 *
 * - 「没有审批」和「无权看审批」是两件事，糊成一句「无记录」就是把权限问题说成业务事实；
 * - 「手工凭证没有来源单据」和「来源类型未识别」也是两件事，前者是正经业务，后者是数据问题；
 * - 空段必须给一句**说得清为什么空**的文案（卡面验收：「任一段缺失时显示无记录而不是空白」）。
 *
 * # 不做的事
 *
 * - 不重算路由：目标页 / 参数 / 深链档位一律来自 VTRACE-1 的 `SOURCE_ROUTE_TABLE`（经
 *   VTRACE-2 的 `resolveSourceDocDisplay` 收敛成「可跳 / 不可跳 + 理由」）；
 * - 不自己推审批进度：时间线只渲染引擎写的 `biz_flow_task` 行；
 * - 不发请求：取数在组件里（`VoucherFullTrace.vue`），本文件只做形状转换。
 */

import type { FlowTraceInstance } from '@/api/business/flow-api';
import type { VoucherEntryLine } from '@/api/business/erp';

import { dash, formatCents, formatMoney, toCents } from '../shared/money';
import type { SourceDocDisplay } from './voucher-source-link';
import { voucherStatusLabel, voucherStatusTheme } from './voucher-logic';

/** 段的四种身份（时间轴从上到下的固定顺序）。 */
export type TraceStepKind = 'source' | 'approval' | 'voucher' | 'entries';

/** 徽标配色（与 TDesign `t-tag` 的 theme 取值一致，组件直接透传）。 */
export type TraceBadgeTheme = 'default' | 'primary' | 'success' | 'warning' | 'danger';

/** 一行内容。 */
export interface TraceLine {
  /** 稳定 key（渲染 `v-for` 用；同一段内唯一） */
  key: string;
  /** 主文本（单号 / 审批人 / 科目） */
  title: string;
  /** 副文本（类型 / 时间 / 节点名…），空串会被渲染层丢掉 */
  metas: string[];
  /** 金额（已格式化，右对齐；没有就不显示） */
  amount?: string;
  /** 徽标 */
  badge?: { text: string; theme: TraceBadgeTheme };
  /** 可跳目标（`null` = 纯文本） */
  link?: { path: string; query: Record<string, string> } | null;
  /** 悬停说明（为什么点不了 / 点过去会落到哪） */
  tip?: string;
  /** 长文本（审批意见 / 分录摘要），单独一行显示 */
  note?: string;
}

/** 一段。 */
export interface TraceStep {
  kind: TraceStepKind;
  /** 段标题（固定四个词，卡面逐字） */
  title: string;
  /** 标题后缀：审批段用来写「上游采购订单 PO-…」这种限定语；空串 = 无后缀 */
  subtitle: string;
  /**
   * 空态文案（**非空 = 这一段没有记录**）。
   *
   * 卡片必须把它显示出来 —— 空白会让人以为是加载失败。
   * 文案要说清「为什么空」：手工凭证 / 未走审批 / 无权查看 / 加载失败 是四种完全不同的事。
   */
  empty: string;
  /** 段内一行行内容（`empty` 非空时恒为 `[]`） */
  lines: TraceLine[];
  /** 段尾汇总（分录段写借贷合计与平衡结论） */
  footer?: string;
}

/** 凭证侧输入（`VoucherDetail` 的子集，够画 ③ 就行）。 */
export interface TraceVoucherInput {
  voucher_no: string;
  voucher_date: string;
  status: string;
  is_red: boolean;
  posted_at?: string | null;
  summary?: string | null;
  reverse_of_no?: string | null;
  reversed_by_no?: string | null;
  total_debit: number | string;
  total_credit: number | string;
}

/** 审批段的输入。 */
export interface TraceApprovalInput {
  /**
   * 这一段审批属于谁。
   *
   * - `''` → 来源单据自己（标题就叫「审批记录」）；
   * - `'上游采购订单 PO-…'` → 收货入库单自己没有审批，走的是上游采购单的审批
   *   （采购入库凭证最常见的形态：收货单由采购单审批放行，然后过账出凭证）。
   */
  owner: string;
  /** 反查到的审批单（发起时间倒序，后端已排） */
  instances: FlowTraceInstance[];
  /** 加载失败的一句话（非空 → 段内显示失败原因，**不说**「无记录」） */
  error: string;
}

/** 全链路输入。 */
export interface FullTraceInput {
  /** ① 来源单据的原值（凭证详情三列） */
  source: {
    type: string | null;
    id: string | null;
    no: string | null;
    docNo: string | null;
  };
  /** ① 的显示结论（VTRACE-2 已算好：可跳 / 不可跳 + 理由） */
  sourceDoc: SourceDocDisplay;
  /**
   * ① 来源单据**自己的**单据日期 / 金额（可选）。
   *
   * 凭证接口里只有「记账依据」（`voucher_date` + 借贷合计），**不是**来源单据本身的
   * 业务日期 / 金额（收货单的入库日期、价税合计可能与之不同）。能拿到真值就传进来
   * （目前只有采购入库走了一次收货单详情），拿不到就退回记账依据并在文案里写明口径
   * —— 绝不把记账金额冒充成单据金额。
   */
  sourceMeta?: { date: string | null; amount: string | null } | null;
  /** ② */
  approval: TraceApprovalInput;
  /** ③ */
  voucher: TraceVoucherInput;
  /** ④ 分录 */
  entries: readonly VoucherEntryLine[];
  /**
   * 辅助核算 id → 人话。
   *
   * 主数据名不在凭证接口里（`aux_supplier_id` 只有 uuid），页面上已经有
   * `auxOptions` 缓存，由调用方注入；解析不出来时退回短 id（绝不显示整串 uuid）。
   */
  auxText?: (dim: 'DEPT' | 'SUPPLIER' | 'MATERIAL' | 'WAREHOUSE', id: string) => string;
}

/** 四个段的固定标题（卡面逐字，验收会 grep）。 */
export const TRACE_STEP_TITLES: Record<TraceStepKind, string> = {
  source: '来源单据',
  approval: '审批记录',
  voucher: '本凭证',
  entries: '分录明细',
};

/** 手工凭证的固定说法（与 `voucher-source-link.ts::MANUAL_NO_SOURCE_TIP` 同一句）。 */
export const TRACE_MANUAL_EMPTY = '手工凭证，无来源单据';

/** 没走审批时的空态文案（这是**业务事实**，不是错误）。 */
export const TRACE_NO_APPROVAL = '无审批记录：该单据未走审批';

/** 加载失败时的空态文案前缀（这是**技术故障**，别和上面那句混）。 */
export const TRACE_APPROVAL_LOAD_FAILED = '审批记录加载失败';

/** 审批实例状态的文案。 */
const INSTANCE_STATUS_LABELS: Record<string, string> = {
  PENDING: '审批中',
  APPROVED: '已通过',
  REJECTED: '已驳回',
  WITHDRAWN: '已撤回',
  CANCELLED: '已终止',
};

/** 审批实例状态的配色。 */
const INSTANCE_STATUS_THEMES: Record<string, TraceBadgeTheme> = {
  PENDING: 'warning',
  APPROVED: 'success',
  REJECTED: 'danger',
  WITHDRAWN: 'default',
  CANCELLED: 'default',
};

/** 待办（时间线上的一行）状态的文案。 */
const TASK_STATUS_LABELS: Record<string, string> = {
  APPROVED: '同意',
  REJECTED: '驳回',
  PENDING: '待审批',
  SKIPPED: '跳过',
  TERMINATED: '已终止',
  TRANSFERRED: '已转办',
  DELEGATED: '已委派',
  CANCELLED: '已取消',
};

/** 待办状态的配色：只给「结论」上色，中间态用 default。 */
const TASK_STATUS_THEMES: Record<string, TraceBadgeTheme> = {
  APPROVED: 'success',
  REJECTED: 'danger',
  PENDING: 'warning',
  SKIPPED: 'default',
  TERMINATED: 'default',
  TRANSFERRED: 'default',
  DELEGATED: 'default',
  CANCELLED: 'default',
};

/**
 * 时间戳 → `YYYY-MM-DD HH:mm`（**本地时区**）。
 *
 * 与 `flow/instance/detail.vue::formatTime` 同一口径：审批记录是「几点几分谁点了同意」，
 * 显示 UTC 会差 8 小时，比不显示更糟。
 */
export function formatTraceTime(value: string | null | undefined): string {
  if (!value) {
    return '';
  }
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return value;
  }
  const pad = (input: number): string => String(input).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(
    date.getHours(),
  )}:${pad(date.getMinutes())}`;
}

/** 审批实例状态文案（未知值原样透出，不吞）。 */
export function instanceStatusLabel(status: string): string {
  return INSTANCE_STATUS_LABELS[status] ?? dash(status);
}

/** 审批实例状态配色。 */
export function instanceStatusTheme(status: string): TraceBadgeTheme {
  return INSTANCE_STATUS_THEMES[status] ?? 'default';
}

/** 待办状态文案（未知值原样透出）。 */
export function taskStatusLabel(status: string): string {
  return TASK_STATUS_LABELS[status] ?? dash(status);
}

/** 待办状态配色。 */
export function taskStatusTheme(status: string): TraceBadgeTheme {
  return TASK_STATUS_THEMES[status] ?? 'default';
}

/** 取第一个非空的定位值（`source_doc_no` 是给人看的那一列）。 */
function sourceNoOf(source: FullTraceInput['source']): string {
  return (source.docNo || source.no || '').trim();
}

/**
 * ① 来源单据。
 *
 * 三种形态：
 * - **能跳**：行 + 链接 + tooltip（点过去会落到详情 / 列表，由 VTRACE-2 判定）；
 * - **有单据但不能跳**（页面未上线 / 缺主键 / 类型未识别）：行，纯文本 + 原因 tooltip；
 * - **没有单据**（手工凭证 / 凭证没记来源）：空态，文案说清是哪一种。
 */
function buildSourceStep(input: FullTraceInput): TraceStep {
  const { source, sourceDoc, voucher } = input;
  const base: TraceStep = {
    kind: 'source',
    title: TRACE_STEP_TITLES.source,
    subtitle: '',
    empty: '',
    lines: [],
  };

  if (sourceDoc.status === 'empty' || (!source.type && !sourceNoOf(source))) {
    return { ...base, empty: '无来源单据（本张凭证未记录来源）' };
  }
  if (sourceDoc.status === 'manual') {
    return { ...base, empty: TRACE_MANUAL_EMPTY };
  }

  const no = sourceNoOf(source);
  const meta = input.sourceMeta;
  const metas: string[] = [];
  if (sourceDoc.label) {
    metas.push(`类型：${sourceDoc.label}`);
  } else if (source.type) {
    metas.push(`类型：${source.type}`);
  }
  metas.push(`单号：${dash(no || null)}`);
  // 单据日期：能拿到来源单据自己的日期就用它（收货单的入库日期），拿不到才退回「记账依据日期」
  // —— 两者可能不是同一天，混着写等于编数据。
  if (meta?.date) {
    metas.push(`单据日期：${meta.date}`);
  } else {
    metas.push(`记账依据日期：${dash(voucher.voucher_date)}`);
  }
  if (no && source.id) {
    metas.push(`单据 id：${source.id.slice(0, 8)}…`);
  }

  return {
    ...base,
    lines: [
      {
        key: 'source-1',
        title: sourceDoc.text || (sourceDoc.label ? `查看${sourceDoc.label}` : dash(source.type)),
        metas,
        amount: formatMoney(meta?.amount ?? voucher.total_debit),
        link: sourceDoc.clickable && sourceDoc.path ? { path: sourceDoc.path, query: sourceDoc.query } : null,
        tip: sourceDoc.tip,
      },
    ],
  };
}

/**
 * ② 审批记录。
 *
 * 一组审批单各占一行「单号 + 状态 + 流程 + 发起人」，下面挂它的时间线
 * （节点 · 审批人 · 结论 · 时间 · 意见）。三档语义严格分开：
 *
 * | 情形 | 显示 |
 * |---|---|
 * | 请求失败 | 「审批记录加载失败：<原因>」 |
 * | 查到了 0 张 | 「无审批记录：该单据未走审批」 |
 * | 查到了但无权看明细 | 单号 + 状态照常显示 +「无权限查看审批明细」 |
 */
function buildApprovalStep(input: FullTraceInput): TraceStep {
  const { owner, instances, error } = input.approval;
  const base: TraceStep = {
    kind: 'approval',
    title: TRACE_STEP_TITLES.approval,
    subtitle: owner,
    empty: '',
    lines: [],
  };

  if (error) {
    return { ...base, empty: `${TRACE_APPROVAL_LOAD_FAILED}：${error}` };
  }
  if (!instances.length) {
    return { ...base, empty: TRACE_NO_APPROVAL };
  }

  const lines: TraceLine[] = [];
  for (const instance of instances) {
    lines.push({
      key: `inst-${instance.id}`,
      title: instance.instance_no,
      metas: [
        instance.flow_name,
        `发起人：${instance.initiator_name || '未知'}`,
        `发起时间：${formatTraceTime(instance.started_at)}`,
        instance.ended_at ? `结束时间：${formatTraceTime(instance.ended_at)}` : '仍在审批中',
      ],
      badge: { text: instanceStatusLabel(instance.status), theme: instanceStatusTheme(instance.status) },
      note: instance.visible ? undefined : '无权限查看审批明细（可到审批中心用单号查）',
    });
    if (!instance.visible) {
      continue;
    }
    instance.tasks.forEach((task, index) => {
      const metas = [task.node_name || task.node_key, task.assignee_name || '未指派'];
      if (task.at) {
        metas.push(formatTraceTime(task.at));
      }
      if (task.is_auto) {
        metas.push('系统代点');
      }
      lines.push({
        key: `task-${instance.id}-${index}`,
        title: `${index + 1}. ${task.node_name || task.node_key}`,
        metas: metas.slice(1),
        badge: { text: taskStatusLabel(task.status), theme: taskStatusTheme(task.status) },
        note: task.comment || undefined,
      });
    });
  }
  return { ...base, lines };
}

/** ③ 本凭证（号 / 日期 / 状态 + 记账时间 + 摘要 + 红冲关系）。 */
function buildVoucherStep(input: FullTraceInput): TraceStep {
  const { voucher } = input;
  const metas = [
    `凭证日期：${dash(voucher.voucher_date)}`,
    `记账时间：${voucher.posted_at ? formatTraceTime(voucher.posted_at) : '未记账'}`,
  ];
  const lines: TraceLine[] = [
    {
      key: 'voucher-1',
      title: voucher.voucher_no,
      metas,
      amount: formatMoney(voucher.total_debit),
      badge: {
        text: voucherStatusLabel(voucher.status),
        theme: voucherStatusTheme(voucher.status),
      },
      note: voucher.summary || undefined,
    },
  ];
  if (voucher.is_red) {
    lines.push({
      key: 'voucher-red',
      title: '红字凭证',
      metas: [`红冲的原凭证：${dash(voucher.reverse_of_no)}`],
      badge: { text: '红字', theme: 'danger' },
    });
  } else if (voucher.reversed_by_no) {
    lines.push({
      key: 'voucher-reversed',
      title: '已被红冲',
      metas: [`红字凭证：${voucher.reversed_by_no}`],
      badge: { text: '已红冲', theme: 'warning' },
    });
  }
  return {
    kind: 'voucher',
    title: TRACE_STEP_TITLES.voucher,
    subtitle: '',
    empty: '',
    lines,
    footer: `借方合计 ${formatMoney(voucher.total_debit)} · 贷方合计 ${formatMoney(voucher.total_credit)}`,
  };
}

/**
 * ④ 分录明细。
 *
 * 一条分录一行：借/贷 + 科目 + 金额 + 摘要 + 辅助核算。空态只在「凭证没有任何分录」时出现
 * —— 那本身就是数据问题，文案直说是「无分录」，不假装正常。
 */
function buildEntriesStep(input: FullTraceInput): TraceStep {
  const { entries, auxText } = input;
  const base: TraceStep = {
    kind: 'entries',
    title: TRACE_STEP_TITLES.entries,
    subtitle: '',
    empty: '',
    lines: [],
  };
  if (!entries.length) {
    return { ...base, empty: '无分录（该凭证没有任何分录行，属数据异常）' };
  }

  let debit = 0;
  let credit = 0;
  const lines: TraceLine[] = entries.map((entry) => {
    const debitCents = toCents(entry.debit_amount);
    const creditCents = toCents(entry.credit_amount);
    debit += debitCents;
    credit += creditCents;
    const isDebit = debitCents !== 0;
    const metas = [`${entry.account_code} ${entry.account_name}`.trim()];
    const dims: Array<['DEPT' | 'SUPPLIER' | 'MATERIAL' | 'WAREHOUSE', string | null]> = [
      ['DEPT', entry.aux_dept_id],
      ['SUPPLIER', entry.aux_supplier_id],
      ['MATERIAL', entry.aux_material_id],
      ['WAREHOUSE', entry.aux_warehouse_id],
    ];
    const labels: Record<string, string> = { DEPT: '部门', SUPPLIER: '供应商', MATERIAL: '物料', WAREHOUSE: '仓库' };
    for (const [dim, id] of dims) {
      if (!id) {
        continue;
      }
      const text = auxText ? auxText(dim, id) : '';
      metas.push(`${labels[dim]}：${text || `${id.slice(0, 8)}…`}`);
    }
    return {
      key: `entry-${entry.id}`,
      title: `${entry.line_no} ${entry.account_code} ${entry.account_name}`.trim(),
      metas: metas.slice(1),
      amount: formatMoney(isDebit ? entry.debit_amount : entry.credit_amount),
      badge: { text: isDebit ? '借' : '贷', theme: isDebit ? 'primary' : 'warning' },
      note: entry.summary || undefined,
    };
  });

  const balanced = debit === credit;
  return {
    ...base,
    lines,
    footer: `${entries.length} 行 · 借 ${formatCents(debit)} / 贷 ${formatCents(credit)} · ${
      balanced ? '借贷平衡' : `不平衡（差 ${formatCents(debit - credit)}）`
    }`,
  };
}

/**
 * 组装四段（固定顺序：来源单据 → 审批记录 → 本凭证 → 分录明细）。
 *
 * 四段**永远都在**，缺数据的段落用 `empty` 说明原因 —— 这比「少一段」更接近真相：
 * 用户需要知道的是「这一段没有」而不是「这一段不适用」。
 */
export function buildFullTrace(input: FullTraceInput): TraceStep[] {
  return [
    buildSourceStep(input),
    buildApprovalStep(input),
    buildVoucherStep(input),
    buildEntriesStep(input),
  ];
}

/** 四段里有几段是有内容的（给 `t-tabs` 上的计数徽标用）。 */
export function filledStepCount(steps: readonly TraceStep[]): number {
  return steps.filter((step) => step.lines.length > 0).length;
}
