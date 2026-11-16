/**
 * 现金流底稿（录入 + 预览）的**纯逻辑**（M1-18 · 上游 V0414~V0417）。
 *
 * 为什么单独一个 `.ts`：`index.vue` / 各面板只负责画，而「金额怎么格式化」「输入框里那串
 * 数字合不合法」「哪些行是改动过的」「勾稽结论怎么显示」这些判据一旦写进模板，`vue-tsc`
 * 抓不到漂移，只有写成函数才能钉住（与 `fx-balance-logic.ts` / `opening-logic.ts` 同一套做法）。
 *
 * 口径与后端逐字对齐（`cashflow_worksheet.rs` / `cashflow_statement.rs`）：
 *
 * - 金额是十进制**字符串**（`NUMERIC(18,4)` → `"1200.0000"`），格式化**不许**经过 `Number()`；
 * - 明细行金额**不许为负**（方向由项目的 INFLOW / OUTFLOW 表达），余额行可负（透支 / 受限资金）；
 * - 金额**最多 4 位小数**：多出的位数 PG 会静默四舍五入，造成「底稿合计 ≠ 凭证合计」的尾差，
 *   所以入口先拒（后端 `check_amount` 的同一判据，前端先拦一次是为了少挨一个往返）；
 * - `null` ≠ 0：报表上期列为 `null` 表示「上期没开账 / 上期没编」，显示占位符而不是 0；
 * - 保存**允许暂时不平**（工作稿），`balanced` 只用来显示结论，**不**拦保存。
 *
 * 常量放这里（不放 `api/…/cashflow-worksheet.ts`）：那个文件 import 了 `@/api/request`，
 * 而 `@/api/request` 读 `import.meta.env`，`tsx --test` 的 node 环境里没有它 —— 单测只能
 * import **不碰运行时依赖**的模块（`import type` 会被擦除，常量放这里才测得动）。
 */
import type {
  CashStatement,
  StatementAmount,
  WorksheetActivityBlock,
  WorksheetCheck,
  WorksheetRow,
  WorksheetSheet,
} from '@/api/business/erp/cashflow-worksheet';
import type { AccountPeriodRow } from '@/api/business/erp';

// ==================== 权限码（与 V0415 / V0417 的 sys_menu.auth_code 逐字一致）====================

/** 读底稿 + 勾稽校验（`GET …/worksheet`、`POST …/worksheet/check`） */
export const PERM_WORKSHEET_LIST = 'business:erp:cashflow-worksheet:list';
/** 录入底稿（`PUT …/worksheet`） */
export const PERM_WORKSHEET_SAVE = 'business:erp:cashflow-worksheet:save';
/** 看由底稿出的表（`GET …/statement`） */
export const PERM_STATEMENT_LIST = 'business:erp:cashflow-statement:list';
/** 导出 / 打印由底稿出的表（`GET …/statement/export`） */
export const PERM_STATEMENT_EXPORT = 'business:erp:cashflow-statement:export';

/** 报表编码（后端 `cashflow_statement::REPORT_CODE`） */
export const CASHFLOW_WORKSHEET_REPORT_CODE = 'CASH_FLOW_WORKSHEET';

// ==================== 枚举文案 ====================

/** 活动编码的顺序（后端 `ACTIVITIES`：经营 → 投资 → 筹资） */
export const ACTIVITY_ORDER = ['OPERATING', 'INVESTING', 'FINANCING'] as const;

/** 活动中文（分区标题用 `activity_name`，这里给短名用在勾稽结论 / 合计行） */
export const ACTIVITY_LABELS: Record<string, string> = {
  OPERATING: '经营活动',
  INVESTING: '投资活动',
  FINANCING: '筹资活动',
};

/** 方向中文 */
export const DIRECTION_LABELS: Record<string, string> = {
  INFLOW: '流入',
  OUTFLOW: '流出',
};

/** 勾稽问题机读码 → 中文短语（后端 `PROBLEM_*`） */
export const PROBLEM_LABELS: Record<string, string> = {
  OPENING_MISSING: '缺期初余额',
  CLOSING_MISSING: '缺期末余额',
  IMBALANCED: '勾稽不闭合',
  EMPTY: '本期未编',
};

/** 金额小数位（与 `amount NUMERIC(18,4)` 同口径） */
export const AMOUNT_SCALE = 4;

/** `NUMERIC(18,4)` 的整数部分上限（14 位；后端 `AMOUNT_ABS_LIMIT`） */
export const AMOUNT_INT_DIGITS = 14;

/** 「没有数」的占位符（上期没开账 / 上期没编这一行），别各写各的 `0` / `--` / `无` */
export const EMPTY_AMOUNT_PLACEHOLDER = '—';

/** 期初余额行的名称（后端 `OPENING_NAME`，页面在没拿到 `opening` 时兜底显示） */
export const OPENING_NAME_FALLBACK = '期初现金及现金等价物余额';

/** 期末余额行的名称（后端 `CLOSING_NAME`） */
export const CLOSING_NAME_FALLBACK = '期末现金及现金等价物余额';

// ==================== 金额 ====================

/**
 * 千分位 + 2 位小数（**十进制字符串四舍五入**，不过 `Number()`）。
 *
 * 与 `report/balance-sheet` / `report/cash-flow` 页面里的实现逐字同算法
 * （`Number('0.595').toFixed(2)` 会得到 `0.59`，报表上少一分钱是事故）。
 * 传 `null` / `''` 返回空串，由调用方决定显示占位符。
 */
export function formatAmount(value: string | null | undefined): string {
  if (value === null || value === undefined || value === '') return '';
  const text = String(value).trim();
  const negative = text.startsWith('-');
  const clean = negative ? text.slice(1) : text;
  const [rawInt, rawFrac = ''] = clean.split('.');
  const intPart = rawInt || '0';
  const frac4 = `${rawFrac}0000`.slice(0, 4);

  let digits = (intPart + frac4.slice(0, 2)).split('').map(Number);
  if (frac4[2] >= '5') {
    let index = digits.length - 1;
    while (index >= 0) {
      digits[index] += 1;
      if (digits[index] === 10) {
        digits[index] = 0;
        index -= 1;
      } else {
        break;
      }
    }
    if (index < 0) digits = [1, ...digits];
  }
  const joined = digits.join('') || '0';
  const cents = joined.slice(-2).padStart(2, '0');
  const yuan = (joined.length > 2 ? joined.slice(0, -2) : '0').replace(/^0+(?=\d)/, '');
  const grouped = yuan.replace(/\B(?=(\d{3})+(?!\d))/g, ',');
  const isZero = /^0+$/.test(joined);
  return `${negative && !isZero ? '-' : ''}${grouped}.${cents}`;
}

/**
 * 金额列的展示（`null` → 占位符，其余走 `formatAmount`）。
 *
 * 报表「上期金额」为 `null` 是「没有数」而不是 0：显示 0 会把「上期没编」画成
 * 「上期是零」，两者对报表是相反的结论。
 */
export function amountOrPlaceholder(value: string | null | undefined): string {
  return value === null || value === undefined || value === '' ? EMPTY_AMOUNT_PLACEHOLDER : formatAmount(value);
}

/**
 * 金额的**规范形**：去掉多余的前导零 / 尾随零，`-0` 归一成 `0`。
 *
 * 用途有二：
 * ① 输入框回显（后端给 `"1200.0000"`，输入框显示 `1200`，人不用数零）；
 * ② 判断「这一行改过没有」（`'1200.0000'` 与 `'1200'` 是同一个数，不能算改动）。
 *
 * 不认识的形状（空串 / 非数字）原样返回 `trim` 结果 —— 不静默改成 0。
 */
export function canonicalAmount(value: string | null | undefined): string {
  if (value === null || value === undefined) return '';
  const text = String(value).trim();
  if (text === '') return '';
  const match = /^([+-]?)(\d*)(?:\.(\d*))?$/.exec(text);
  if (!match) return text;
  const [, sign, intRaw = '', fracRaw = ''] = match;
  const intPart = intRaw.replace(/^0+(?=\d)/, '') || '0';
  const fracPart = fracRaw.replace(/0+$/, '');
  const body = fracPart ? `${intPart}.${fracPart}` : intPart;
  const isZero = /^0*$/.test(intPart) && fracPart === '';
  return sign === '-' && !isZero ? `-${body}` : body;
}

/** 输入框回显：已录行给规范形，未录行给空串（空 = 还没编，不是 0） */
export function inputAmount(value: string | null | undefined): string {
  return canonicalAmount(value);
}

/** 两个金额**数值上**是否相等（走规范形比较，`'1200.0000'` == `'1200'`） */
export function sameAmount(left: string | null | undefined, right: string | null | undefined): boolean {
  return canonicalAmount(left) === canonicalAmount(right);
}

/** 金额输入的解析结果 */
export type AmountParse =
  /** 没填（= 这一项不改；要表达「本期为零」请填 0） */
  | { kind: 'empty' }
  | { kind: 'ok'; value: string }
  | { kind: 'error'; message: string };

/**
 * 解析一个金额输入框（同后端 `check_amount` 的判据）。
 *
 * 拒收的四类（都是会静默错账的形状）：
 * - 非十进制数字（`1e3` / `1,200` / `¥1200` / `1 200`）；
 * - 小数位 > 4（PG 会静默四舍五入 → 底稿合计与凭证合计差尾差）；
 * - 整数位 > 14（`NUMERIC(18,4)` 放不下，PG 直接 22003）；
 * - 明细行为负（方向已由配置的 INFLOW / OUTFLOW 表达，负数会让流入合计出现负流入、
 *   与流出栏互相抵消 —— 余额行不受此限，它本来就允许负）。
 */
export function normalizeAmount(text: string, allowNegative: boolean): AmountParse {
  const raw = String(text ?? '').trim();
  if (raw === '') return { kind: 'empty' };
  if (!/^[+-]?\d+(\.\d+)?$/.test(raw)) {
    return { kind: 'error', message: `金额只能填十进制数字（不带千分位、币种符号或科学计数法），收到「${raw}」` };
  }
  const negative = raw.startsWith('-');
  const body = raw.replace(/^[+-]/, '');
  const [intPart, fracPart = ''] = body.split('.');
  if (intPart.replace(/^0+/, '').length > AMOUNT_INT_DIGITS) {
    return { kind: 'error', message: `整数部分最多 ${AMOUNT_INT_DIGITS} 位（NUMERIC(18,4) 的位宽），收到「${raw}」` };
  }
  if (fracPart.length > AMOUNT_SCALE) {
    return {
      kind: 'error',
      message: `金额最多 ${AMOUNT_SCALE} 位小数（多出的位数数据库会静默四舍五入，造成尾差），收到「${raw}」`,
    };
  }
  const value = canonicalAmount(raw);
  if (negative && value !== '0' && !allowNegative) {
    return {
      kind: 'error',
      message: `明细行金额不能为负（${raw}）：方向由项目的「流入 / 流出」决定，负数会和流出栏互相抵消`,
    };
  }
  return { kind: 'ok', value };
}

// ==================== 期间 ====================

/** 期间状态中文（`NOT_OPENED` = 这个月还没开账） */
export function periodStatusText(status: string | null | undefined): string {
  switch (status) {
    case 'OPEN':
      return '未结账';
    case 'CLOSED':
      return '已结账';
    case 'NOT_OPENED':
      return '未开账';
    default:
      return status ? String(status) : '未知';
  }
}

/**
 * 期间下拉：只列**已建立**的期间（`NOT_OPENED` 选了也一定 400/置灰），新的在前。
 *
 * 已结账期间照样列出 —— 历史底稿要能打开来看（GET 不受期间闸限制，只是 `editable=false`）。
 */
export function periodOptions(rows: AccountPeriodRow[]): { label: string; value: string }[] {
  return rows
    .filter((row) => row.status !== 'NOT_OPENED')
    .slice()
    .sort((a, b) => b.period_code.localeCompare(a.period_code))
    .map((row) => ({
      label: `${row.period_code}（${row.status === 'CLOSED' ? '已结账' : '未结账'}）`,
      value: row.period_code,
    }));
}

/** 时间戳显示（`2026-11-16 13:04:00`）；解析不了就原样返回 */
export function formatDateTime(value: string | null | undefined): string {
  if (!value) return '';
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return String(value);
  const pad = (n: number) => String(n).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
}

// ==================== 底稿：草稿与提交 ====================

/** 一行草稿（金额 / 备注都是输入框里的原始字符串） */
export interface DraftRow {
  /** 输入框原文；空串 = 还没编 / 不改 */
  amount: string;
  remark: string;
}

/** 整张底稿的草稿（键 = `item_code`） */
export interface DraftState {
  opening: string;
  openingRemark: string;
  closing: string;
  closingRemark: string;
  rows: Record<string, DraftRow>;
}

/** 活动分区（页面按这个顺序画「一、经营活动…」三块） */
export interface WorksheetSection {
  activity: string;
  /** 后端标题（`一、经营活动产生的现金流量`）；拿不到时用 `ACTIVITY_LABELS` 兜底 */
  name: string;
  inflow: WorksheetRow[];
  outflow: WorksheetRow[];
  /** 该活动的小计（后端算好的；没有时 `null`） */
  block: WorksheetActivityBlock | null;
}

/** 行排序：`sort_order` 升序，同序按 `item_code`（与后端的稳定排序同口径） */
export function sortWorksheetRows(rows: WorksheetRow[]): WorksheetRow[] {
  return rows
    .slice()
    .sort((a, b) => a.sort_order - b.sort_order || a.item_code.localeCompare(b.item_code));
}

/**
 * 把底稿行按「活动 → 流入 / 流出」分组（沿用后端 `activities` 的三个块顺序）。
 *
 * 配置表里已停用但底稿有历史行的项目照样进分组（`item_active=false`，页面标「已停用」）：
 * 静默丢掉就是丢数。
 */
export function groupWorksheetRows(sheet: WorksheetSheet): WorksheetSection[] {
  const blocks = new Map<string, WorksheetActivityBlock>();
  for (const block of sheet.activities ?? []) {
    blocks.set(block.code, block);
  }
  const order: string[] = [];
  for (const code of ACTIVITY_ORDER) {
    if (blocks.has(code) || sheet.rows.some((row) => row.activity === code)) {
      order.push(code);
    }
  }
  for (const row of sheet.rows) {
    if (!order.includes(row.activity)) order.push(row.activity);
  }
  return order.map((activity) => {
    const rows = sortWorksheetRows(sheet.rows.filter((row) => row.activity === activity));
    const block = blocks.get(activity) ?? null;
    return {
      activity,
      name: block?.name ?? ACTIVITY_LABELS[activity] ?? activity,
      inflow: rows.filter((row) => row.direction === 'INFLOW'),
      outflow: rows.filter((row) => row.direction === 'OUTFLOW'),
      block,
    };
  });
}

/**
 * 从后端返回的底稿生成草稿。
 *
 * 两个刻意的细节：
 * ① **未录入的配置项金额留空**而不是 `'0'` —— 空 = 「还没编」，0 = 「本期确实是零」，
 *    这两件事对勾稽是相反的结论；
 * ② 金额走 `canonicalAmount`（`"1200.0000"` → `1200`），输入框里不让人数零。
 */
export function buildDraft(sheet: WorksheetSheet): DraftState {
  const rows: Record<string, DraftRow> = {};
  for (const row of sheet.rows) {
    rows[row.item_code] = {
      amount: row.entered ? canonicalAmount(row.amount) : '',
      remark: row.remark ?? '',
    };
  }
  return {
    rows,
    opening: sheet.opening ? canonicalAmount(sheet.opening.amount) : '',
    openingRemark: sheet.opening?.remark ?? '',
    closing: sheet.closing ? canonicalAmount(sheet.closing.amount) : '',
    closingRemark: sheet.closing?.remark ?? '',
  };
}

/** 一行是否改过（金额按数值比较，备注按原文比较） */
function rowChanged(draft: DraftRow | undefined, base: DraftRow | undefined): boolean {
  if (!draft || !base) return false;
  return (
    !sameAmount(draft.amount, base.amount) ||
    (draft.remark ?? '').trim() !== (base.remark ?? '').trim()
  );
}

/** 改动过的明细行编码（按底稿行顺序，保证提交顺序稳定、可断言） */
export function changedItemCodes(draft: DraftState, sheet: WorksheetSheet): string[] {
  const base = buildDraft(sheet);
  return sortWorksheetRows(sheet.rows)
    .filter((row) => rowChanged(draft.rows[row.item_code], base.rows[row.item_code]))
    .map((row) => row.item_code);
}

/** 整张草稿是否有改动（含期初 / 期末余额两行） */
export function isDraftDirty(draft: DraftState, sheet: WorksheetSheet): boolean {
  const base = buildDraft(sheet);
  const balanceChanged =
    !sameAmount(draft.opening, base.opening) ||
    !sameAmount(draft.closing, base.closing) ||
    (draft.openingRemark ?? '').trim() !== (base.openingRemark ?? '').trim() ||
    (draft.closingRemark ?? '').trim() !== (base.closingRemark ?? '').trim();
  return balanceChanged || changedItemCodes(draft, sheet).length > 0;
}

/** 提交体的构造结果：`null` = 没有任何改动（不该发请求） */
export type SavePayloadResult =
  | null
  | { error: string }
  | { payload: { period: string; opening?: { amount: string; remark: string | null }; closing?: { amount: string; remark: string | null }; items: { item_code: string; amount: string; remark: string | null }[] } };

/**
 * 构造 `PUT …/worksheet` 的提交体：**只发改动过的行**。
 *
 * 为什么不整个表发一遍：后端 `items` 是非空即 upsert 的语义，把 12 个配置项（含从没编过的）
 * 全发一遍 = 把没录过的行写成「已录 0」，`entered` 位被污染，「本期没编这一行」这个信息就没了。
 *
 * 校验在这里做（而不是等后端 400）：金额空 → 不动；金额不合法 → 返回人话错误（带项目名）；
 * 期初 / 期末允许负、明细行不允许负（与后端同判据）。三者全空 → 返回 `null`。
 */
export function buildSavePayload(period: string, draft: DraftState, sheet: WorksheetSheet): SavePayloadResult {
  const base = buildDraft(sheet);
  const result: Exclude<SavePayloadResult, null | { error: string }>['payload'] = { period, items: [] };

  if (!sameAmount(draft.opening, base.opening) || (draft.openingRemark ?? '').trim() !== (base.openingRemark ?? '').trim()) {
    const parsed = normalizeAmount(draft.opening, true);
    if (parsed.kind === 'error') return { error: `期初余额：${parsed.message}` };
    if (parsed.kind === 'empty') return { error: '期初余额填了备注却没有金额：请补金额（本期无现金请填 0）' };
    result.opening = { amount: parsed.value, remark: draft.openingRemark.trim() || null };
  }

  if (!sameAmount(draft.closing, base.closing) || (draft.closingRemark ?? '').trim() !== (base.closingRemark ?? '').trim()) {
    const parsed = normalizeAmount(draft.closing, true);
    if (parsed.kind === 'error') return { error: `期末余额：${parsed.message}` };
    if (parsed.kind === 'empty') return { error: '期末余额填了备注却没有金额：请补金额（本期无现金请填 0）' };
    result.closing = { amount: parsed.value, remark: draft.closingRemark.trim() || null };
  }

  for (const code of changedItemCodes(draft, sheet)) {
    const row = sheet.rows.find((item) => item.item_code === code);
    const rowDraft = draft.rows[code];
    if (!row || !rowDraft) continue;
    const parsed = normalizeAmount(rowDraft.amount, false);
    if (parsed.kind === 'error') {
      return { error: `「${row.item_name}」（${row.item_code}）：${parsed.message}` };
    }
    if (parsed.kind === 'empty') {
      return {
        error: `「${row.item_name}」（${row.item_code}）已清空金额：要表达「本期为零」请填 0，清空不等于 0`,
      };
    }
    result.items.push({ item_code: row.item_code, amount: parsed.value, remark: rowDraft.remark.trim() || null });
  }

  const hasBalance = Boolean(result.opening || result.closing);
  if (!hasBalance && result.items.length === 0) return null;
  return { payload: result };
}

/**
 * 构造 `POST …/worksheet/check` 的提交体（**不落库**的试算）。
 *
 * 与保存同一份草稿：只带改动过的值，其余让后端拿已存底稿；`require_balanced` 只在
 * 「出表前复核」时为 `true`（页面的「试算」用 `false`，要的是差额与问题清单，不是 400）。
 */
export function buildCheckPayload(
  period: string,
  draft: DraftState,
  sheet: WorksheetSheet,
  options: { requireBalanced?: boolean } = {},
): { error: string } | { payload: { period: string; opening?: { amount: string; remark: string | null }; closing?: { amount: string; remark: string | null }; items: { item_code: string; amount: string; remark: string | null }[]; require_balanced: boolean } } {
  const base = buildDraft(sheet);
  const payload: {
    period: string;
    opening?: { amount: string; remark: string | null };
    closing?: { amount: string; remark: string | null };
    items: { item_code: string; amount: string; remark: string | null }[];
    require_balanced: boolean;
  } = { period, items: [], require_balanced: Boolean(options.requireBalanced) };

  if (!sameAmount(draft.opening, base.opening)) {
    const parsed = normalizeAmount(draft.opening, true);
    if (parsed.kind === 'error') return { error: `期初余额：${parsed.message}` };
    if (parsed.kind === 'ok') payload.opening = { amount: parsed.value, remark: draft.openingRemark.trim() || null };
  }
  if (!sameAmount(draft.closing, base.closing)) {
    const parsed = normalizeAmount(draft.closing, true);
    if (parsed.kind === 'error') return { error: `期末余额：${parsed.message}` };
    if (parsed.kind === 'ok') payload.closing = { amount: parsed.value, remark: draft.closingRemark.trim() || null };
  }
  for (const code of changedItemCodes(draft, sheet)) {
    const row = sheet.rows.find((item) => item.item_code === code);
    const rowDraft = draft.rows[code];
    if (!row || !rowDraft) continue;
    const parsed = normalizeAmount(rowDraft.amount, false);
    if (parsed.kind === 'error') return { error: `「${row.item_name}」（${row.item_code}）：${parsed.message}` };
    if (parsed.kind === 'ok') payload.items.push({ item_code: row.item_code, amount: parsed.value, remark: rowDraft.remark.trim() || null });
  }
  return { payload };
}

// ==================== 勾稽结论 ====================

/** 勾稽结论的色调（`ok` 绿 / `bad` 红）—— 只用于展示，不用于拦保存 */
export function checkTone(check: Pick<WorksheetCheck, 'balanced'>): 'ok' | 'bad' {
  return check.balanced ? 'ok' : 'bad';
}

/** 勾稽问题机读码 → 中文短语（未知码原样返回，别把新码吞掉） */
export function problemLabel(code: string): string {
  return PROBLEM_LABELS[code] ?? code;
}

/**
 * 勾稽问题的一句话正文（**去掉和后端 `message` 重复的前缀**）。
 *
 * 后端 `problems[].message` 已经自带「勾稽不闭合：期初 … 差 100.0000」这种以短语开头的句子，
 * 页面又要在前面画一个中文短语标签，直接拼会变成「勾稽不闭合：勾稽不闭合：期初 …」。
 * 这里只在 `message` 真的以该短语开头时才剥掉，不认识的码 / 不匹配的文案原样返回。
 */
export function problemText(problem: { code: string; message: string }): string {
  const label = problemLabel(problem.code);
  const message = problem.message ?? '';
  if (!message.startsWith(label)) return message;
  return message.slice(label.length).replace(/^[：:，,、\s]+/, '');
}

/** 差额的人话（`null` = 缺一端算不出；0 = 闭合） */
export function diffText(diff: string | null | undefined): string {
  if (diff === null || diff === undefined || diff === '') return EMPTY_AMOUNT_PLACEHOLDER;
  return canonicalAmount(diff) === '0' ? '0' : formatAmount(diff);
}

/** 出表勾稽是否两条等式都成立（后端 `balanced` 已合并两条，这里只做展示兜底） */
export function statementCheckTone(check: Pick<CashStatement['check'], 'balanced'>): 'ok' | 'bad' {
  return check.balanced ? 'ok' : 'bad';
}

/**
 * 导出前的前端拦截文案（`null` = 可以导）。
 *
 * 为什么前端也要拦一次：出表闸的**权威**在后端（未闭合 400 + `66087`），但那条错误走的是
 * `responseType: 'blob'` 的失败路径 —— 响应体被 axios 包成 Blob，全局拦截器读不到 `msg`，
 * 用户只能看到「请求参数不正确」这类通用话术。页面手里已经有 `check` 与 `problems`，
 * 直接把它们拼成人话拦下来，比让用户看错误码表强；后端闸照旧保留（改库 / 并发改数时仍会拦）。
 */
export function exportBlockedReason(
  check: Pick<WorksheetCheck, 'balanced' | 'message'>,
  problems: { code: string; message: string }[],
): string | null {
  if (check.balanced) return null;
  const detail = problems.map((problem) => problemText(problem)).join('；');
  return detail ? `${check.message}（${detail}）` : check.message;
}

/** 金额对里「上期有没有数」（页面决定上期列画占位符还是金额） */
export function previousAvailable(pair: Pick<StatementAmount, 'previous'>): boolean {
  return pair.previous !== null && pair.previous !== undefined;
}
