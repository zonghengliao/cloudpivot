/**
 * 框架协议页面纯逻辑（M4-11）：**协议台账 + 执行量**。
 *
 * # 为什么展示件单独放 `.ts`
 *
 * `.vue` 里的函数 Node 单测加载不了（SFC 要编译），而这一页最容易翻车的恰恰是**展示口径**：
 * `null` 显示成 0（「不限量」被读成「一点都不剩」）、负数剩余被截断到 0（超上限被藏起来）、
 * 用百分比反推状态（99.995% 那一格翻错）。所以判据全部收在这里，
 * 由 `frontend/tests/unit/erp-framework-agreement.test.ts` 一条条钉住。
 *
 * # 与后端逐条对齐的口径（后端 `framework_agreement_execution_dto.rs` 头注释）
 *
 * ```text
 * ① 封顶 / 剩余 / 执行率 / 金额上限 的 null = 不限，**不是** 0
 * ② remaining_qty < 0 = 超上限，必须原样显形（进度条封顶 100，倍数写文案）
 * ③ exec_state 由后端给（UNLIMITED/NOT_STARTED/PARTIAL/DONE/OVER），**不用 pct 反推**
 * ④ booked_drift ≠ 0 = 回写快照与单据链分叉，低调提示，不弹错
 * ⑤ 作废 / 软删的采购单也在下钻里（counted=false + verdict 原因）
 * ```
 *
 * # 为什么金额显示要做字符串取整（`moneyText`）
 *
 * 后端 `Decimal` 出参是 `"300.0000"` 这种定长文本。直接 `Number()` 再 `toFixed(2)` 在亿元级
 * 会丢精度（双精度 15–17 位有效数字），而单据金额不该有「显示层造的误差」。
 * `moneyText` 只做**字符串层面的四舍五入**（半进位），不做任何浮点运算。
 */
import type {
  AgreementExecItem,
  AgreementExecRow,
  AgreementStatus,
  DecimalText,
  ExecSource,
  ExecState,
  PriceMode,
  ValidityState,
} from '@/api/business/erp/framework-agreement';

// ==================== 域常量 ====================

/** 每页条数（与采购计划 / 供应商列表同口径） */
export const PAGE_SIZE = 20;

/** 关键字长度上限（后端 `MAX_KEYWORD_LEN = 64`，超了 400） */
export const MAX_KEYWORD = 64;

/** 协议状态下拉（顺序与后端 `fwa::STATUSES` 一致；值里**不放** `''`，TDesign 把空串当未选） */
export const STATUS_OPTIONS: { value: AgreementStatus; label: string }[] = [
  { value: 'DRAFT', label: '拟稿' },
  { value: 'SUBMITTED', label: '审批中' },
  { value: 'APPROVED', label: '生效' },
];

/** 有效期状态下拉（顺序与后端 `VALIDITY_STATES` 一致） */
export const VALIDITY_OPTIONS: { value: ValidityState; label: string }[] = [
  { value: 'ACTIVE', label: '生效中' },
  { value: 'NOT_STARTED', label: '未生效' },
  { value: 'EXPIRED', label: '已过期' },
  { value: 'DRAFT', label: '拟稿' },
  { value: 'SUBMITTED', label: '审批中' },
];

// ==================== 基础取值 ====================

/** 「不限」的统一文案（`null` 的唯一正确读法） */
export const UNLIMITED_TEXT = '不限';

/** 数值占位：空值给 `—`（与 ERP 其它页同款，不写 0） */
export function toNumberOrNull(value: number | string | null | undefined): number | null {
  if (value === null || value === undefined) {
    return null;
  }
  if (typeof value === 'string' && value.trim() === '') {
    return null;
  }
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

/**
 * 原样展示（`—` 兜空）。
 *
 * 用于**协议价 / 单价 / 数量**这类「库里几位就几位」的字段：审计要能对上原文。
 */
export function decimalText(value: DecimalText | null | undefined): string {
  if (value === null || value === undefined || value === '') {
    return '—';
  }
  return String(value);
}

/**
 * 金额展示：**字符串层面**舍到 2 位小数（半进位），不用浮点。
 *
 * `"300.0000"` → `"300.00"`、`"1234.5678"` → `"1234.57"`、`"1.999"` → `"2.00"`、
 * `"12345678901234.5678"` → `"12345678901234.57"`（双精度那条路 16 位后会开始掉尾数）。
 * 负数保留符号（超上限的剩余金额要能看出来）。
 */
export function moneyText(value: DecimalText | null | undefined): string {
  if (value === null || value === undefined || value === '') {
    return '—';
  }
  const raw = String(value).trim();
  const negative = raw.startsWith('-');
  const body = negative ? raw.slice(1) : raw;
  if (!/^\d*(\.\d*)?$/.test(body) || body === '' || body === '.') {
    // 非数字原样返回（后端若改了出参形状，这里不会把它变成 NaN 文本）
    return raw;
  }
  const [intPart = '0', fracPart = ''] = body.split('.');
  // 整数位去前导零（但至少留一位），小数位补到 3 位（第 3 位只用于判进位）
  let intText = intPart.replace(/^0+(?=\d)/, '') || '0';
  const frac3 = fracPart.padEnd(3, '0');
  let fracText = frac3.slice(0, 2);
  if (Number(frac3[2]) >= 5) {
    if (fracText === '99') {
      fracText = '00';
      intText = incrementDigits(intText);
    } else {
      fracText = String(Number(fracText) + 1).padStart(2, '0');
    }
  }
  return `${negative ? '-' : ''}${intText}.${fracText}`;
}

/**
 * 十进制数字串 +1（纯字符串进位，`Math` 在 16 位后会开始丢精度）。
 *
 * `'129'` → `'130'`、`'99'` → `'100'`、`'0'` → `'1'`。
 */
function incrementDigits(digits: string): string {
  const chars = digits.split('');
  let index = chars.length - 1;
  while (index >= 0) {
    if (chars[index] === '9') {
      chars[index] = '0';
      index -= 1;
    } else {
      chars[index] = String(Number(chars[index]) + 1);
      return chars.join('');
    }
  }
  return `1${chars.join('')}`;
}

/** 封顶类字段：`null` = 不限（**绝不**显示 0） */
export function capText(value: DecimalText | null | undefined): string {
  if (toNumberOrNull(value) === null) {
    return UNLIMITED_TEXT;
  }
  return decimalText(value);
}

/** 金额封顶：`null` = 不限，否则按金额格式（2 位） */
export function capMoneyText(value: DecimalText | null | undefined): string {
  if (toNumberOrNull(value) === null) {
    return UNLIMITED_TEXT;
  }
  return moneyText(value);
}

/**
 * 剩余量 / 剩余金额。
 *
 * * `null` → 「不限」（不是 0）
 * * 负数 → 「超 X」（超上限要显形，**不要**截断到 0）
 * * 其余 → 原样
 */
export function remainingText(value: DecimalText | null | undefined): string {
  const parsed = toNumberOrNull(value);
  if (parsed === null) {
    return UNLIMITED_TEXT;
  }
  if (parsed < 0) {
    return `超 ${decimalText(String(value).replace(/^-/, ''))}`;
  }
  return decimalText(value);
}

/** 剩余金额（同 `remainingText`，多一层 2 位小数） */
export function remainingMoneyText(value: DecimalText | null | undefined): string {
  const parsed = toNumberOrNull(value);
  if (parsed === null) {
    return UNLIMITED_TEXT;
  }
  if (parsed < 0) {
    return `超 ${moneyText(String(value).replace(/^-/, ''))}`;
  }
  return moneyText(value);
}

/**
 * 执行率百分比文案。
 *
 * `null` = **不限量**（没有分母），显示「不限」—— 显示 `0.00%` 会被读成「一点没做」。
 * 后端已经舍到 2 位（`exec_qty_rate_pct`），这里只加百分号，不再动数。
 */
export function pctText(pct: DecimalText | null | undefined): string {
  if (toNumberOrNull(pct) === null) {
    return UNLIMITED_TEXT;
  }
  return `${pct}%`;
}

/**
 * 进度条百分比（0–100 的 number）。
 *
 * **超上限也封顶 100**：`t-progress` 画过 100 会出界；真实倍数在 `pctText` 里
 * （`150.00%`），状态在 `exec_state` 标签里 —— 进度条不负责表达「超了」。
 */
export function progressPercent(pct: DecimalText | null | undefined): number {
  const value = toNumberOrNull(pct);
  if (value === null || value <= 0) {
    return 0;
  }
  return Math.min(100, Math.round(value * 100) / 100);
}

/** 进度条状态（`UNLIMITED` / `NOT_STARTED` 不染色，避免「灰条」被当成失败） */
export function progressStatus(state: ExecState | string): 'success' | 'error' | 'active' | undefined {
  switch (state) {
    case 'DONE':
      return 'success';
    case 'OVER':
      return 'error';
    case 'PARTIAL':
      return 'active';
    default:
      return undefined;
  }
}

// ==================== 标签与主题 ====================

/** 协议状态中文（后端 `status_label` 缺失时兜底；未知名原样返回，绝不 panic） */
export function agreementStatusLabel(status: string): string {
  switch (status) {
    case 'DRAFT':
      return '拟稿';
    case 'SUBMITTED':
      return '审批中';
    case 'APPROVED':
      return '生效';
    default:
      return status;
  }
}

/** 协议状态标签主题（与执行量视图并排看时颜色一致） */
export function statusTheme(status: string): 'default' | 'warning' | 'success' {
  switch (status) {
    case 'SUBMITTED':
      return 'warning';
    case 'APPROVED':
      return 'success';
    default:
      return 'default';
  }
}

/** 有效期状态中文（后端 `validity_state_label` 缺失时兜底） */
export function validityLabel(state: string): string {
  switch (state) {
    case 'DRAFT':
      return '拟稿';
    case 'SUBMITTED':
      return '审批中';
    case 'NOT_STARTED':
      return '未生效';
    case 'ACTIVE':
      return '生效中';
    case 'EXPIRED':
      return '已过期';
    default:
      return state;
  }
}

/**
 * 有效期状态标签主题。
 *
 * `EXPIRED` 用 `danger`（对采购来说「协议过期了还在用」是事故），
 * `NOT_STARTED` 用 `default`（只是还没到日子，不是错）。
 */
export function validityTheme(state: string): 'default' | 'success' | 'warning' | 'danger' {
  switch (state) {
    case 'ACTIVE':
      return 'success';
    case 'EXPIRED':
      return 'danger';
    case 'SUBMITTED':
      return 'warning';
    default:
      return 'default';
  }
}

/** 执行状态标签主题（五态全覆蓋，`UNLIMITED` 用 primary 与「未开工」区分） */
export function execStateTheme(state: string): 'default' | 'primary' | 'success' | 'warning' | 'danger' {
  switch (state) {
    case 'UNLIMITED':
      return 'primary';
    case 'PARTIAL':
      return 'warning';
    case 'DONE':
      return 'success';
    case 'OVER':
      return 'danger';
    default:
      return 'default';
  }
}

/** 执行状态中文（后端 `exec_state_label` 缺失时兜底） */
export function execStateLabel(state: string): string {
  switch (state) {
    case 'UNLIMITED':
      return '不限量';
    case 'NOT_STARTED':
      return '未开工';
    case 'PARTIAL':
      return '部分执行';
    case 'DONE':
      return '已用满';
    case 'OVER':
      return '已超限';
    default:
      return state;
  }
}

/** 定价方式中文 */
export function priceModeLabel(mode: PriceMode | string): string {
  switch (mode) {
    case 'FIXED':
      return '固定价';
    case 'FLOATING':
      return '随行就市';
    default:
      return String(mode);
  }
}

/** 含税口径（协议价的比较口径，金额上限跟着它走） */
export function priceTaxText(includesTax: boolean): string {
  return includesTax ? '含税价' : '不含税价';
}

/**
 * 价格上浮容忍度。
 *
 * `0` = 下单价**不得高于**协议价（M4-08 硬拦）；`> 0` 才是「允许上浮 N%」。
 * 写成「可上浮 0.00%」会让人以为是「完全不卡价」，所以给一句人话。
 */
export function toleranceText(value: DecimalText | null | undefined): string {
  const parsed = toNumberOrNull(value);
  if (parsed === null) {
    return '—';
  }
  if (parsed <= 0) {
    return '不得高于协议价';
  }
  return `可上浮 ${(parsed * 100).toFixed(2)}%`;
}

/** 税率展示：后端存分数（0.13），界面要 `13%` */
export function taxRateText(rate: DecimalText | null | undefined): string {
  const parsed = toNumberOrNull(rate);
  if (parsed === null) {
    return '—';
  }
  return `${(parsed * 100).toFixed(2).replace(/\.?0+$/, '')}%`;
}

/**
 * 有效期一句话。
 *
 * * 拟稿 / 审批中 → 直接给状态（**不看日期**：草稿日期区间对也不生效）
 * * 其余按 `days_left` 的正负说人话（`days_left` 由后端按 CST 当日算，含首尾）
 */
export function validityText(row: {
  validity_state: ValidityState | string;
  days_left: number;
}): string {
  switch (row.validity_state) {
    case 'DRAFT':
      return '拟稿（不生效）';
    case 'SUBMITTED':
      return '审批中（不生效）';
    case 'NOT_STARTED':
      return `未生效 · 还有 ${row.days_left} 天`;
    case 'EXPIRED':
      return `已过期 ${Math.abs(row.days_left)} 天`;
    case 'ACTIVE':
      return `生效中 · 还剩 ${row.days_left} 天`;
    default:
      return String(row.validity_state);
  }
}

/**
 * 有效期「剩余天数」一句话（**列表用**：状态已经在标签里，这里只说天数，别重复）。
 *
 * 拟稿 / 审批中 没有天数概念（日期还没成为判据），返回空串让 `cell-sub` 不占位。
 */
export function validityLeftText(row: { validity_state: ValidityState | string; days_left: number }): string {
  switch (row.validity_state) {
    case 'ACTIVE':
      return `还剩 ${row.days_left} 天`;
    case 'NOT_STARTED':
      return `还有 ${row.days_left} 天`;
    case 'EXPIRED':
      return `已过期 ${Math.abs(row.days_left)} 天`;
    default:
      return '';
  }
}

/** 有效期区间文案 */
export function periodText(row: { start_date: string; end_date: string }): string {
  return `${row.start_date} ~ ${row.end_date}`;
}

/**
 * 行状态计数文案。
 *
 * 五个数并排给，**不合并**：「不限量」的行既不是未开工也不是部分执行，
 * 合并成「其它」会让人以为有行漏算了。
 */
export function lineCountsText(row: {
  item_count: number;
  unlimited_items: number;
  not_started_items: number;
  partial_items: number;
  done_items: number;
  over_items: number;
}): string {
  return [
    `共 ${row.item_count} 行`,
    `不限 ${row.unlimited_items}`,
    `未开工 ${row.not_started_items}`,
    `部分 ${row.partial_items}`,
    `已用满 ${row.done_items}`,
    `超限 ${row.over_items}`,
  ].join(' · ');
}

/** 单头「已用金额」标签：协议价含税就要看含税金额（与金额上限同一口径） */
export function usedAmountLabel(includesTax: boolean): string {
  return includesTax ? '已用金额（含税）' : '已用金额（不含税）';
}

/** 单头已用金额（按协议价口径取那一列，别自己挑） */
export function usedAmountOf(row: AgreementExecRow): string {
  return moneyText(row.executed_amount_capped);
}

/**
 * 回写分叉提示（`booked_drift ≠ 0`）。
 *
 * 视图现算是真相，回写列是 M4-08 下单时写的快照；两者不等说明中间有人绕过 handler
 * 改过库（数据修复 / 导入 / 失败残留）。**这不是错误**，所以只给一行灰字，
 * 不弹窗、不染红。
 */
export function driftText(item: { booked_qty: DecimalText; booked_drift: DecimalText }): string {
  const drift = toNumberOrNull(item.booked_drift);
  if (drift === null || drift === 0) {
    return '';
  }
  return `回写快照 ${decimalText(item.booked_qty)}（差 ${decimalText(item.booked_drift)}）`;
}

/** 是否有回写分叉（给单元格加个低调标记） */
export function hasDrift(item: { booked_drift: DecimalText }): boolean {
  const drift = toNumberOrNull(item.booked_drift);
  return drift !== null && drift !== 0;
}

/** 下钻判定标签主题：计入 = success，不计入 = default（不染红：作废本身是正常业务） */
export function sourceVerdictTheme(source: { counted: boolean }): 'success' | 'default' {
  return source.counted ? 'success' : 'default';
}

/** 采购单号 + 软删标记（软删单在后端仍留着，前端要说清「已删」而不是空号） */
export function sourceOrderText(source: { order_no: string; order_deleted_at: string | null }): string {
  return source.order_deleted_at ? `${source.order_no}（已删）` : source.order_no;
}

/** 下钻行主键：「采购单 + 行号」（同一张单可能多行引用同一协议行） */
export function sourceKey(source: Pick<ExecSource, 'order_id' | 'line_no'>): string {
  return `${source.order_id}:${source.line_no}`;
}

/**
 * 某个物料行的下钻来源（后端一把给全协议的 sources，页面按 `agreement_item_id` 分）。
 *
 * 返回 `[]` 时不是错误：这一行还没被任何采购单引用。
 */
export function sourcesOf(items: ExecSource[], agreementItemId: string): ExecSource[] {
  return items.filter((source) => source.agreement_item_id === agreementItemId);
}

/** 下钻汇总：计入 N 行 / 释放 M 行（`counted=false` 的是作废与软删） */
export function sourcesSummaryText(sources: ExecSource[]): string {
  const counted = sources.filter((source) => source.counted).length;
  const released = sources.length - counted;
  return `计入 ${counted} 行 · 释放 ${released} 行`;
}

/** 单头「单数」文案（计入 + 已释放并排给，解释「为什么已用量没涨」） */
export function docCountText(row: { order_doc_count: number; released_doc_count: number }): string {
  return `${row.order_doc_count} 张${row.released_doc_count > 0 ? `（释放 ${row.released_doc_count}）` : ''}`;
}

/**
 * 行状态计数（**列表用短版**：只列非零的桶）。
 *
 * 五段全排在列表里会被列宽截成「共 2 行 · 不限 0 · 未开工 0 …」（演练截图实测），
 * 而列表这一格要看的是「有没有超限」；完整五段留给详情抽屉（`lineCountsText`）。
 */
export function lineCountsBrief(row: {
  item_count: number;
  unlimited_items: number;
  not_started_items: number;
  partial_items: number;
  done_items: number;
  over_items: number;
}): string {
  const parts = [`共 ${row.item_count} 行`];
  if (row.over_items > 0) {
    parts.push(`超限 ${row.over_items}`);
  }
  if (row.unlimited_items > 0) {
    parts.push(`不限 ${row.unlimited_items}`);
  }
  if (row.partial_items > 0) {
    parts.push(`部分 ${row.partial_items}`);
  }
  if (row.done_items > 0) {
    parts.push(`用满 ${row.done_items}`);
  }
  if (row.not_started_items > 0) {
    parts.push(`未开工 ${row.not_started_items}`);
  }
  return parts.join(' · ');
}

/**
 * 时间戳展示（`2026-11-17T22:20:50.123Z` → `2026-11-17 22:20`）。
 *
 * 只截串、不解析时区：后端给的是 RFC3339，秒与毫秒对排查没帮助，却会把描述表撑爆。
 */
export function timeText(value: string | null | undefined): string {
  if (!value) {
    return '—';
  }
  const match = /^(\d{4}-\d{2}-\d{2})[T ](\d{2}:\d{2})/.exec(value);
  return match ? `${match[1]} ${match[2]}` : value;
}

/** 物料行标题（行号 + 编码 + 名称） */
export function materialText(item: { material_code: string; material_name: string }): string {
  return `${item.material_code} ${item.material_name}`.trim();
}

/** 协议行在列表里的物料摘要（`A01 钢板 等 2 项`），列表只有计数、详情才有逐行 */
export function itemSummaryText(row: { item_count: number; unlimited_items: number }): string {
  if (row.item_count === 0) {
    return '无物料范围';
  }
  return row.unlimited_items > 0
    ? `${row.item_count} 项（不限 ${row.unlimited_items}）`
    : `${row.item_count} 项`;
}

/** 物料行「用量」摘要（列表 / 详情共用） */
export function usedQtyText(item: { executed_qty: DecimalText; max_qty: DecimalText | null }): string {
  return `${decimalText(item.executed_qty)} / ${capText(item.max_qty)}`;
}

/** 协议行金额摘要（已用 / 封顶） */
export function usedAmountItemText(row: AgreementExecItem): string {
  if (toNumberOrNull(row.cap_amount) === null) {
    return moneyText(row.executed_amount_capped);
  }
  return `${moneyText(row.executed_amount_capped)} / ${moneyText(row.cap_amount)}`;
}

// ==================== 筛选与入参 ====================

/** 协议台账筛选态 */
export interface LedgerFilterState {
  /** 关键字：协议号 / 名称 / 供应商名 / 经办人 */
  keyword: string;
  /** 协议状态（`''` = 全部） */
  status: AgreementStatus | '';
  /** 有效期状态（`''` = 全部） */
  validity_state: ValidityState | '';
}

/** 执行量筛选态（多一个「只看超限」） */
export interface ExecFilterState extends LedgerFilterState {
  /** 只看有行超上限的协议 */
  over_only: boolean;
}

/** 台账初值 */
export function emptyLedgerFilter(): LedgerFilterState {
  return { keyword: '', status: '', validity_state: '' };
}

/** 执行量初值 */
export function emptyExecFilter(): ExecFilterState {
  return { ...emptyLedgerFilter(), over_only: false };
}

/**
 * 分页入参拼装（两个 Tab 共用一套判据）。
 *
 * * `keyword` trim 后为空 → **整个键不传**（传 `?keyword=` 后端按空串处理还行，但
 *   其它页面的教训是「空串筛选查出 0 行」，统一不传最省事）；
 * * `status` / `validity_state` 空串 → 不传（后端空串也当没筛，但少一个键少一次歧义）；
 * * `over_only` 只有 `true` 才传（后端默认 false，传 `false` 是噪音）；
 * * `page_size` 恒传（后端默认 20，页面要能改）；
 * * 关键字超长**不截断**：后端 400 的报文比「悄悄截断后查出一堆不相干的行」有用。
 */
export function buildQuery(
  filter: LedgerFilterState & { over_only?: boolean },
  page: number,
  pageSize: number,
): Record<string, unknown> {
  const query: Record<string, unknown> = {
    page: Math.max(1, Math.trunc(page) || 1),
    page_size: Math.max(1, Math.trunc(pageSize) || PAGE_SIZE),
  };
  const keyword = filter.keyword.trim();
  if (keyword) {
    query.keyword = keyword;
  }
  if (filter.status) {
    query.status = filter.status;
  }
  if (filter.validity_state) {
    query.validity_state = filter.validity_state;
  }
  if (filter.over_only === true) {
    query.over_only = true;
  }
  return query;
}

/** 台账入参（不带 over_only，`buildQuery` 会忽略它） */
export function buildLedgerQuery(
  filter: LedgerFilterState,
  page: number,
  pageSize: number,
): Record<string, unknown> {
  return buildQuery(filter, page, pageSize);
}

/** 执行量入参 */
export function buildExecQuery(
  filter: ExecFilterState,
  page: number,
  pageSize: number,
): Record<string, unknown> {
  return buildQuery(filter, page, pageSize);
}

/**
 * 关键字是否超长（超长时页面先提示，再让后端也拦一道）。
 *
 * 返回 `null` 表示合法；返回字符串是给人看的提示。
 */
export function keywordWarning(filter: { keyword: string }): string | null {
  const length = filter.keyword.trim().length;
  if (length > MAX_KEYWORD) {
    return `关键字最长 ${MAX_KEYWORD} 个字符（当前 ${length}）`;
  }
  return null;
}

/** 空态文案（有筛选 / 没筛选给两种，别让人以为「系统没数据」） */
export function emptyListText(filter: LedgerFilterState, mode: 'ledger' | 'execution'): string {
  const filtered =
    filter.keyword.trim() !== '' || filter.status !== '' || filter.validity_state !== '';
  if (filtered) {
    return '没有符合筛选条件的框架协议 —— 换个关键字或清空筛选再看';
  }
  return mode === 'ledger'
    ? '还没有框架协议'
    : '还没有可统计执行量的框架协议（协议要在库里存在，执行量才有行）';
}

/** 详情抽屉标题 */
export function detailTitle(row: AgreementExecRow | null): string {
  if (!row) {
    return '协议详情';
  }
  return `${row.agreement_no} ${row.agreement_name}`;
}

/** 供应商展示（快照优先还是现名优先：约定 13 —— 业务展示用**现名**） */
export function supplierText(row: { supplier_display_name: string; supplier_name: string | null }): string {
  return row.supplier_display_name || row.supplier_name || '—';
}

/** 经办信息（`采购部 / 张三`，缺一头就只显示另一头） */
export function ownerText(row: { dept_name: string | null; owner_name: string | null }): string {
  const parts = [row.dept_name, row.owner_name].filter((part): part is string => !!part);
  return parts.length ? parts.join(' / ') : '—';
}
