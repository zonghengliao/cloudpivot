/**
 * 成本核算页的纯逻辑（ARCH-9 §6.2.2 五步向导 · COST-B3-11）。
 *
 * 只有纯函数 / 常量，**不 import Vue、不 import 请求层**，所以 `pnpm test:unit`
 * （`tsx --test`）能直接打。页面 `index.vue` 只做「取数 + 摆位置」。
 *
 * # 五步向导（设计 §6.2.2；卡面写作「料工费确认 / 试算」）
 *
 * ```
 * ① 选期间 → ② 料工费确认（缺口自查）→ ③ 试算（成本计算单）→ ④ 提交审批 → ⑤ 结算
 * ```
 *
 * 每一「步」都能由批次状态反推（`stepOfStatus`）：用户从列表点进来时不该被要求
 * 从第 ① 步重新走 —— 状态已经说明走到哪了。
 *
 * # 两条易错口径
 *
 * 1. **`DRAFT` 阶段的单头 `total_*` 恒 0**（那是结算落库值）：页面上的「投入合计」
 *    一律取 `items_summary`（按计算单现算），别用 `detail.total_cost` —— 那会在
 *    提交审批前显示 0，用户以为算错了；
 * 2. **可去状态以后端 `next_statuses` 为准，前端只做「按权限隐藏」**：后端读的是
 *    管理端可禁用的**有效边表**，前端自己按状态硬编码就会在「边被禁用」时给出
 *    一个点下去 409 的按钮（死按钮比没按钮更糟）。
 */

import type {
  CostPeriodDetail,
  CostPeriodStatus,
  CostPeriodSummary,
  CostSheetItem,
} from '@/api/business/erp/cost-period';
import {
  COST_PERIOD_STATUS_LABELS,
  COST_PERIOD_STATUS_THEMES,
  LABOR_SOURCE_LABELS,
  REVERSE_REASON_MIN,
  SETTLE_KEYWORD,
  type LaborSource,
} from '@/api/business/erp/cost-period-kinds';
import { formatScaled, scaledInt, sumDecimal } from '@/views/business/erp/shared/decimal';
import { formatMoney } from '@/views/business/erp/shared/money';

// ==================== 五步定义 ====================

/** 向导的一步 */
export interface WizardStep {
  /** 机器键（data-testid 后缀） */
  key: string;
  /** 步骤标题（≤6 字，`t-steps` 一行放得下） */
  title: string;
  /** 一句话说明（步骤下方小字） */
  hint: string;
}

/** 五步（顺序即数据流顺序，设计 §6.2.2） */
export const WIZARD_STEPS: WizardStep[] = [
  { key: 'period', title: '选期间', hint: '只列未结账期间；一个期间同时只有一个活跃批次' },
  { key: 'check', title: '料工费确认', hint: '料 / 工 / 费自查缺口，不阻断，但要能解释' },
  { key: 'calc', title: '试算', hint: '成本计算单：对象 × 料工费 × 约当 × 单位成本 × 完工/在制' },
  { key: 'approve', title: '提交审批', hint: '财务经理 → 财务总监两级顺序审批' },
  { key: 'settle', title: '结算', hint: '生成生产入库单 + 三张凭证；结算后仍可红冲' },
];

/** 最后一步的下标 */
export const LAST_STEP = WIZARD_STEPS.length - 1;

/**
 * 批次状态 → 应该停在第几步。
 *
 * - 没有批次（新开）→ ①（选期间）；
 * - `DRAFT` → ③（试算）：① ② 已经走完，用户回来是改数/看结果；
 * - `SUBMITTED` → ④（提交审批）：看审批进度、可撤回；
 * - `APPROVED` / `SETTLED` / `CLOSED` → ⑤（结算）：看结算结果 / 红冲；
 * - `CANCELLED` / `REVERSED` → ①（重开）：红冲/作废后同期间可重建新批次。
 */
export function stepOfStatus(status: CostPeriodStatus | null | undefined): number {
  switch (status) {
    case 'DRAFT':
      return 2;
    case 'SUBMITTED':
      return 3;
    case 'APPROVED':
    case 'SETTLED':
    case 'CLOSED':
      return LAST_STEP;
    default:
      return 0;
  }
}

/** 批次是否已终结（作废 / 红冲）：这两态不能再提交、结算，只能重开） */
export function isFinalStatus(status: CostPeriodStatus | null | undefined): boolean {
  return status === 'CANCELLED' || status === 'REVERSED';
}

/** 状态中文名（未知状态原样返回，别显示空白） */
export function statusLabel(status: string | null | undefined): string {
  if (!status) {
    return '—';
  }
  return COST_PERIOD_STATUS_LABELS[status as CostPeriodStatus] ?? status;
}

/** 状态配色 */
export function statusTheme(
  status: string | null | undefined,
): 'default' | 'primary' | 'warning' | 'success' | 'danger' {
  if (!status) {
    return 'default';
  }
  return COST_PERIOD_STATUS_THEMES[status as CostPeriodStatus] ?? 'default';
}

/** 人工来源中文 */
export function laborSourceLabel(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  return LABOR_SOURCE_LABELS[value as LaborSource] ?? value;
}

// ==================== 动作可用性（前端只做「按状态 + 权限」两层） ====================

/** 可建批次 / 可重算（只有 `DRAFT` 能重算，设计 §6.2.2 ③） */
export function canRecalc(status: CostPeriodStatus | null | undefined): boolean {
  return status === 'DRAFT';
}

/** 可提交审批（`DRAFT → SUBMITTED`） */
export function canSubmit(status: CostPeriodStatus | null | undefined): boolean {
  return status === 'DRAFT';
}

/** 可撤回（`SUBMITTED → DRAFT`；实例已落终态时后端自愈） */
export function canWithdraw(status: CostPeriodStatus | null | undefined): boolean {
  return status === 'SUBMITTED';
}

/** 可结算（`APPROVED → SETTLED`；`SUBMITTED` 时提示「需审批通过后才能结算」） */
export function canSettle(status: CostPeriodStatus | null | undefined): boolean {
  return status === 'APPROVED';
}

/** 可红冲（只有 `SETTLED`；`CLOSED` 的边由状态机定，前端不擅自放开） */
export function canReverse(status: CostPeriodStatus | null | undefined): boolean {
  return status === 'SETTLED';
}

/** 可作废（只有 `DRAFT`） */
export function canCancel(status: CostPeriodStatus | null | undefined): boolean {
  return status === 'DRAFT';
}

/** 可改人工来源 / 备注（只有 `DRAFT`） */
export function canEdit(status: CostPeriodStatus | null | undefined): boolean {
  return status === 'DRAFT';
}

/**
 * 「结算前置未满足」的人话（设计 §6.2.2 错误表最后一行）。
 *
 * `SUBMITTED` 时结算按钮要置灰并说明原因 —— 不是「按钮不见了」，
 * 用户会以为功能没上线。
 */
export function settleBlockedHint(status: CostPeriodStatus | null | undefined): string {
  if (status === 'APPROVED') {
    return '审批通过：可以结算（会生成生产入库单与三张凭证），结算后仍可红冲';
  }
  if (status === 'SUBMITTED') {
    return '批次状态是「已提交待审」，需审批通过后才能结算（财务经理 → 财务总监两级）';
  }
  if (status === 'DRAFT') {
    return '草稿还不能结算：先提交审批（第 ④ 步），审批通过后才能结算';
  }
  if (isFinalStatus(status)) {
    return `批次已${statusLabel(status)}，不能结算：同期间可新建下一批（作废/红冲不占位）`;
  }
  if (status === 'SETTLED' || status === 'CLOSED') {
    return '已结算：如需更正请走红冲（会生成红字凭证并回冲库存）';
  }
  return '当前状态不能结算';
}

// ==================== 期间 ====================

/** 期间格式（与库层 `ck_biz_cost_period_code` 同款） */
const PERIOD_RE = /^\d{4}-(0[1-9]|1[0-2])$/;

/** 期间是否合法 `yyyy-MM` */
export function periodIsValid(period: string | null | undefined): boolean {
  return PERIOD_RE.test((period ?? '').trim());
}

/** 去空白（空串 = 没选） */
export function normalizePeriod(period: string | null | undefined): string {
  return (period ?? '').trim();
}

/** 「下一步」的放行条件 */
export interface NextGate {
  /** 已选期间 */
  period: string;
  /** 期间已结账（设计 §6.2.2 ①：需先反结账） */
  periodClosed: boolean;
  /** 是否已有批次（无批次时第 ③ 步会显示「建批次」）。 */
  hasBatch: boolean;
}

/**
 * 第 `step` 步能不能点「下一步」。
 *
 * - ① → ②：期间合法且未结账（有一批活跃批次也允许继续 —— 缺口自查对期间有意义，
 *   用户可能想去 ② 先看缺口再决定要不要开批次）；
 * - ② → ③：**始终允许**（缺口不阻断，设计 §6.2.2 ② 原文）；
 * - ③ → ④：必须有批次（提交审批是对着批次提交的）；
 * - ④ → ⑤：始终允许（结算按钮自己按状态置灰 + 说明）。
 */
export function canNextFrom(step: number, gate: NextGate): boolean {
  if (step === 0) {
    return periodIsValid(gate.period) && !gate.periodClosed;
  }
  if (step === 1) {
    return true;
  }
  if (step === 2) {
    return gate.hasBatch;
  }
  return true;
}

// ==================== 高危确认 ====================

/**
 * 确认词是否一致（**必须逐字全等**，去首尾空白；大小写不敏感只对英文有用，
 * 这里两个词都是中文，所以只去空白）。
 */
export function keywordMatches(input: string, keyword: string): boolean {
  return (input ?? '').trim() === keyword;
}

/** 结算确认词是否输入正确 */
export function settleKeywordOk(input: string): boolean {
  return keywordMatches(input, SETTLE_KEYWORD);
}

/** 红冲原因字数（按**字符**算，不是字节：中文 1 字算 1） */
export function reasonLength(reason: string | null | undefined): number {
  return [...(reason ?? '').trim()].length;
}

/** 红冲原因是否够长（后端 `REVERSE_REASON_MIN` = 10） */
export function reverseReasonOk(reason: string | null | undefined): boolean {
  return reasonLength(reason) >= REVERSE_REASON_MIN;
}

/** 原因校验的人话（`null` = 通过） */
export function reasonHint(reason: string | null | undefined, min = REVERSE_REASON_MIN): string | null {
  const len = reasonLength(reason);
  if (len === 0) {
    return `原因必填（至少 ${min} 个字）：这个动作会影响已入账数据，原因要能追溯到「为什么」`;
  }
  if (len < min) {
    return `原因至少 ${min} 个字（现在 ${len} 个）`;
  }
  return null;
}

// ==================== 计算单：合计与行明细 ====================

/** 定点小数位（金额 4 位，与库层 `NUMERIC(18,4)` 一致） */
export const AMOUNT_SCALE = 4;
/** 单价小数位（`NUMERIC(18,6)`） */
export const UNIT_SCALE = 6;

/** 合计（**按计算单现算**，不用单头 `total_*`；见文件头口径 1） */
export function summaryOf(items: readonly CostSheetItem[] | null | undefined): CostPeriodSummary {
  const rows = items ?? [];
  const pick = (field: keyof CostSheetItem): string =>
    sumDecimal(
      rows.map((item) => String(item[field] ?? '0')),
      AMOUNT_SCALE,
    );
  return {
    object_count: rows.length,
    total_material: pick('mat_cur'),
    total_labor: pick('labor_cur'),
    total_overhead: pick('oh_cur'),
    total_cost: sumDecimal(
      rows.flatMap((item) => [item.mat_cur, item.labor_cur, item.oh_cur]),
      AMOUNT_SCALE,
    ),
    total_finished: pick('finished_cost'),
    total_wip: pick('wip_end_cost'),
  };
}

/**
 * 单行成本守恒：`完工转出 + 期末在制 = 期初在制 + 料 + 工 + 费`。
 *
 * 库层 `ck_biz_cost_item_cons` 已经硬拦，这里是**只读的界面自证**（用户能一眼看出
 * 这张单是不是被手工改过库）。
 */
export function itemConserved(item: CostSheetItem): boolean {
  const left = sumDecimal([item.finished_cost, item.wip_end_cost], AMOUNT_SCALE);
  const right = sumDecimal(
    [item.wip_beg_cost, item.mat_cur, item.labor_cur, item.oh_cur],
    AMOUNT_SCALE,
  );
  return left === right;
}

/** 明细面板的一行（展开行里显示） */
export interface DetailRow {
  /** 分组：数量 / 本期投入 / 单位成本 / 期初与期末 */
  group: string;
  /** 字段名 */
  label: string;
  /** 显示值（数量 4 位、金额 2 位、单价 6 位） */
  value: string;
}

/** 数量显示（去掉无意义的尾 0，最多 4 位；`"2805.0000"` → `2805`） */
export function qtyText(value: string | null | undefined): string {
  const n = Number(value ?? 0);
  return Number.isFinite(n) ? String(Number(n.toFixed(4))) : '0';
}

/** 单价显示（6 位小数，`NUMERIC(18,6)`） */
export function unitText(value: string | null | undefined): string {
  return formatScaled(scaledInt(value, UNIT_SCALE), UNIT_SCALE);
}

/** 展开行的明细（步③「试算结果可看明细」的落点） */
export function itemDetailRows(item: CostSheetItem): DetailRow[] {
  const money = (value: string | null | undefined): string => formatMoney(value);
  const unit = (value: string | null | undefined): string => unitText(value);
  const num = (value: string | null | undefined): string => qtyText(value);
  return [
    { group: '数量', label: '期初在制数量', value: num(item.wip_beg_qty) },
    { group: '数量', label: '本期完工数量', value: num(item.completed_qty) },
    { group: '数量', label: '期末在制数量', value: num(item.wip_end_qty) },
    { group: '数量', label: '期末在制完工进度', value: `${num(item.completion_pct)}%` },
    { group: '数量', label: '约当产量（材料）', value: num(item.equiv_material) },
    { group: '数量', label: '约当产量（转换成本）', value: num(item.equiv_conversion) },
    { group: '金额', label: '期初在制成本', value: money(item.wip_beg_cost) },
    { group: '金额', label: '本期材料投入', value: money(item.mat_cur) },
    { group: '金额', label: '本期人工投入', value: money(item.labor_cur) },
    { group: '金额', label: '本期制造费用投入', value: money(item.oh_cur) },
    { group: '金额', label: '完工转出成本', value: money(item.finished_cost) },
    { group: '金额', label: '期末在制成本', value: money(item.wip_end_cost) },
    { group: '单位成本', label: '单位材料成本', value: unit(item.unit_material) },
    { group: '单位成本', label: '单位人工成本', value: unit(item.unit_labor) },
    { group: '单位成本', label: '单位制造费用', value: unit(item.unit_overhead) },
    { group: '单位成本', label: '完工入库单价', value: unit(item.unit_finished_cost) },
  ];
}

/** 试算结果的一句话摘要（表头/步③顶部） */
export function calcSummaryText(detail: CostPeriodDetail | null | undefined): string {
  if (!detail) {
    return '还没有试算结果';
  }
  if (detail.items.length === 0) {
    return '本期还没有成本计算单：计算结果由后端「计算」流程写入（设计 #27 入口尚未上线，见卡内汇报）';
  }
  const summary = summaryOf(detail.items);
  return (
    `${summary.object_count} 个成本对象 · 投入 ${formatMoney(summary.total_cost)}` +
    ` · 完工转出 ${formatMoney(summary.total_finished)}` +
    ` · 期末在制 ${formatMoney(summary.total_wip)}`
  );
}

// ==================== 结算 / 红冲的计划文案 ====================

/**
 * 结算确认框的「将要发生什么」（设计 §6.2.2 ⑤：列出 N 张入库单 + 3 张凭证 + 金额合计）。
 *
 * 张数取**计算单行数**（一个成本对象一张生产入库单），凭证恒 3 张：
 * 人工计提 / 制造费用分摊 / 完工结转。金额取计算单现算合计（不是单头 0）。
 */
export function settlePlanText(
  detail: Pick<CostPeriodDetail, 'items'> | null | undefined,
): string {
  const items = detail?.items ?? [];
  const summary = summaryOf(items);
  const vouchers = [
    summary.total_labor === '0.0000' ? null : '人工计提',
    summary.total_overhead === '0.0000' ? null : '制造费用分摊',
    '完工结转',
  ].filter((name): name is string => name !== null);
  return (
    `将生成 ${items.length} 张生产入库单 + ${vouchers.length} 张凭证` +
    `（${vouchers.join(' / ')}），投入合计 ${formatMoney(summary.total_cost)}，` +
    `完工转出 ${formatMoney(summary.total_finished)}`
  );
}

/**
 * 步⑤ 顶部那句话：**未结算说「将生成」，已结算说「已生成」**。
 *
 * 已结算批次若继续用未来时（「将生成 0 张入库单」）会让用户以为结算还没跑；
 * 已结算的数量/金额取**单头落库值**（`object_count` / `total_finished`），因为
 * 结算后计算单行可能已被后续重算清掉，明细现算不再是当时结算的口径。
 */
export function settleSummaryText(
  detail:
    | Pick<CostPeriodDetail, 'status' | 'items' | 'object_count' | 'total_finished'>
    | null
    | undefined,
): string {
  if (!detail) {
    return '';
  }
  if (detail.status === 'SETTLED' || detail.status === 'CLOSED') {
    return (
      `已结算：${detail.object_count} 张生产入库单 + 3 张凭证（人工计提 / 制造费用 / 完工结转），` +
      `完工转出 ${formatMoney(detail.total_finished)}`
    );
  }
  if (detail.status === 'REVERSED') {
    return `已红冲：红字凭证已生成、库存已回冲、入库单已作废（累计完工转出 ${formatMoney(detail.total_finished)} 已冲回）`;
  }
  return settlePlanText(detail);
}

/** 红冲确认框的「将要发生什么」（设计 §4.8 反向路径 ①–⑤） */
export function reversePlanText(detail: CostPeriodDetail | null | undefined): string {
  if (!detail) {
    return '红冲将反向撤销这次结算';
  }
  const money = formatMoney(detail.total_finished || '0');
  return (
    `将为批次 ${detail.batch_no} 生成红字凭证（冲回人工/费用/完工结转共 3 张蓝字），` +
    `回冲 ${detail.object_count} 个成本对象的库存（金额 ${money}），` +
    `生产入库单作废，批次置「已红冲」，计算单回到可重算`
  );
}

/** 提交审批前的流程预览（设计 §6.2.2 ④「弹流程预览（两级审批人）」） */
export const FLOW_PREVIEW_LINES: string[] = [
  '提交后批次状态变「已提交待审」，数据冻结（不能在页面改数）',
  '第 1 级：财务经理审批（角色 FINANCE_MANAGER）',
  '第 2 级：财务总监审批（角色 FINANCE_DIRECTOR）',
  '两级都通过 → 状态变「审批通过」，才能结算（第 ⑤ 步）',
  '被驳回 / 撤回 → 回到「草稿」，可改数后重新提交',
];

/** 审批进度区标题（与其它单据同一个词） */
export const APPROVAL_PROGRESS_TITLE = '审批进度';

/** 结算后三张凭证的展示顺序（空 id 不显示） */
export function voucherEntries(
  detail: Pick<
    CostPeriodDetail,
    'labor_voucher_id' | 'overhead_voucher_id' | 'finish_voucher_id'
  > | null
    | undefined,
): Array<{ label: string; id: string }> {
  if (!detail) {
    return [];
  }
  const pairs: Array<[string, string | null]> = [
    ['人工计提凭证', detail.labor_voucher_id],
    ['制造费用凭证', detail.overhead_voucher_id],
    ['完工结转凭证', detail.finish_voucher_id],
  ];
  return pairs
    .filter((pair): pair is [string, string] => Boolean(pair[1]))
    .map(([label, id]) => ({ label, id }));
}

/** 向导标题 */
export function wizardTitle(period: string, batchNo: string | null | undefined): string {
  const head = batchNo ? `成本核算 · ${batchNo}` : '成本核算 · 新建批次';
  return period ? `${head}（期间 ${period}）` : head;
}

/** 列表行 → 「该做什么」的一句话（列表操作列 tooltip） */
export function nextActionText(status: CostPeriodStatus | null | undefined): string {
  switch (status) {
    case 'DRAFT':
      return '继续核算（试算 / 提交审批）';
    case 'SUBMITTED':
      return '等审批；可撤回';
    case 'APPROVED':
      return '可以结算';
    case 'SETTLED':
      return '已结算；可红冲';
    case 'CLOSED':
      return '期间已结账（只读）';
    case 'CANCELLED':
      return '已作废（同期间可重开）';
    case 'REVERSED':
      return '已红冲（同期间可重开）';
    default:
      return '—';
  }
}

/**
 * 成本计算单导出文件名（与后端 `cost_period_sheet::file_name` 逐字一致）。
 *
 * 为什么前端也要拼一份：`saveBlob` 只认前端给的文件名（浏览器不会用
 * `Content-Disposition` 里服务端的名字）。两份实现必须同形，否则「后端导出叫
 * 成本计算单_CS-202610-0001.xlsx、前端存成 CS-202610-0001.xlsx」这种不一致会让人
 * 以为是两个文件。批次号本身含期间，所以只拼批次号。
 */
export function costSheetFileName(batchNo: string | null | undefined): string {
  const name = (batchNo || '').trim() || '未命名批次';
  return `成本计算单_${name}.xlsx`;
}
