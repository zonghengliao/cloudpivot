/**
 * 销售返利「开红字发票」纯逻辑（M3-19F 前端 / 后端 M3-17）—— 判断与文案都在这里，`.vue` 只渲染。
 *
 * # 口径（全抄后端，前端不重算一分钱）
 *
 * | 项 | 真口径 | 出处 |
 * |----|--------|------|
 * | 能开的单 | `status = 'DRAFT'` 且 `rebate_amount > 0` | `rebate_invoice_dto::ensure_invoiceable` / `split_invoice` |
 * | 金额拆分 | 含税反算：`不含税 = −round(应返 / (1 + 税率/100), 2)`，`税额 = 价税合计 − 不含税` | `split_invoice` |
 * | 三列符号 | 全为负（`ck_biz_invoice_sign`：`amount < 0 && tax ≤ 0`）；**展示别再补负号** | V0298 |
 * | 凭证 | 借 5001 收入 / 借 2221.02 销项税（税额 0 时不出这行）/ 贷 2241.05 其他应付款-销售返利 | `rebate_invoice_handler::write_rebate_voucher` |
 * | 税率 | **百分数**（13 = 13%），0~100，最多 4 位小数；省 = 13 | `validate_tax_rate` |
 * | 备注 | ≤ 200 字；空白归一成 `null`（后端用自动备注） | `normalize_remark` |
 * | 状态机 | `DRAFT ──开票──▶ SETTLED`；`SETTLED` / `CANCELLED` 一律 409 + `66093` | `ensure_invoiceable` |
 *
 * # 三条会让人误判的
 *
 * 1. **试算返回 `can_generate = false` 不是接口失败**（HTTP 200）：预览的作用就是
 *    「点开就能看见为什么不能开」，所以 `blockers` 要原样列在弹窗里，而不是弹一个错误框。
 * 2. **票号唯一由库兜**（`uk_biz_srs_invoice`）：两个页签 / 两个人同时开同一张单，
 *    后到的那位拿到 `409 + 66093`（状态闸先拦）或 `409` 唯一约束冲突（并发窗口）。
 *    两种都要刷列表再看，**不能白屏**，也不能让用户以为「点了没反应」再点一次。
 * 3. **开票日期决定期间闸**：日期落在已结账月份 → `66095`。当天开票撞上的最常见情形是
 *    「上月已结账、现在补开上月返利」，所以弹窗默认今天，但允许改日期 —— 改日期后必须
 *    **重新试算**（期间闸是按日期查的，不是按结算期间查的）。
 */
import type { PrimaryTableCol } from 'tdesign-vue-next';
import { resolveErrorMessage } from '@/constants/error-messages';
import { formatMoney, toNumber, todayString } from '@/views/business/erp/shared/money';
import type {
  RebateInvoicePreview,
  RebateInvoiceReq,
  RebateInvoiceRow,
  RebateInvoiceVoucherLine,
  RebateSettlementRow,
} from '@/api/business/erp/rebate';

/** 默认税率（百分数；`rebate_invoice_dto::DEFAULT_TAX_RATE_PERCENT`） */
export const INVOICE_TAX_RATE_DEFAULT = 13;

/** 税率上界（百分数；`ck_biz_invoice_tax_rate` 是 0~100） */
export const INVOICE_TAX_RATE_MAX = 100;

/** 税率小数位上限（库列 `NUMERIC(7,4)`） */
export const INVOICE_TAX_RATE_SCALE = 4;

/** 备注上限（`rebate_invoice_dto::MAX_REMARK_LEN`） */
export const INVOICE_REMARK_MAX = 200;

/** 三列拆分的展示表列（金额都在金额列里，`align: right`） */
export const INVOICE_SPLIT_COLUMNS: PrimaryTableCol[] = [
  { colKey: 'label', title: '票面列', width: 130 },
  { colKey: 'amount', title: '金额', width: 150, align: 'right' },
  { colKey: 'meaning', title: '含义', minWidth: 200, ellipsis: true },
];

/** 凭证分录的展示表列（借方 / 贷方分两列，一眼能看出平衡） */
export const INVOICE_VOUCHER_COLUMNS: PrimaryTableCol[] = [
  { colKey: 'side', title: '方向', width: 70 },
  { colKey: 'account', title: '科目', minWidth: 200, ellipsis: true },
  { colKey: 'debit', title: '借方', width: 130, align: 'right' },
  { colKey: 'credit', title: '贷方', width: 130, align: 'right' },
  { colKey: 'summary', title: '摘要', minWidth: 140, ellipsis: true },
];

// ==================== 按钮显隐 / 可开判据 ====================

/**
 * 这一行要不要**渲染**「开票」入口。
 *
 * 只有草稿才渲染：`SETTLED` 已在「红字发票」列显示票号（再给个不能点的按钮是噪音），
 * `CANCELLED` 不产生返利，压根不该有入口。
 */
export function showInvoiceAction(
  row: Pick<RebateSettlementRow, 'status'>,
): boolean {
  return row.status === 'DRAFT';
}

/**
 * 这一行现在**能不能开票**（后端仍会再判一次）。
 *
 * 两条硬条件：草稿 + 应返 > 0。金额为 0 的草稿**入口照样点得开**（弹窗里看得到
 * 后端的 `66094` 原文 + 码表人话），但「确认开票」按钮是禁用的 ——
 * 「可用」落在**能不能真的把票开出去**，而不是「按钮能不能点」：
 * 入口直接消失或点不动都会被当成权限问题，比看见原因更糟。
 */
export function canInvoice(
  row: Pick<RebateSettlementRow, 'status' | 'rebate_amount'>,
): boolean {
  return row.status === 'DRAFT' && toNumber(row.rebate_amount) > 0;
}

/** 「开票」按钮的 tooltip：可用说口径，不可用说原因 + 出路 */
export function invoiceGateTip(
  row: Pick<RebateSettlementRow, 'status' | 'rebate_amount' | 'invoice_no'>,
): string {
  if (canInvoice(row)) {
    return '先看票面试算（三列拆分 + 凭证分录），确认后生成销项红字发票并把结算单转「已结算」';
  }
  if (row.status !== 'DRAFT') {
    return row.status === 'SETTLED'
      ? `已开过红字发票${row.invoice_no ? `（${row.invoice_no}）` : ''}：重复开票会重复冲减收入与销项税，要调整请走红冲 / 调账`
      : '已作废的结算单不产生返利，不能再开票（要返利请重新算一张草稿单）';
  }
  return `应返金额 ${formatMoney(row.rebate_amount)} ≤ 0：0 元红字票没有业务含义（后端会拒 66094）。确认这期确实没有返利请作废该单，或修正后重算`;
}

// ==================== 表单 ====================

/** 开票弹窗的表单draft（金额相关不进表单：金额由后端算） */
export interface InvoiceFormDraft {
  /** 开票日期 `yyyy-MM-dd`（决定期间闸） */
  invoice_date: string;
  /** 税率（百分数） */
  tax_rate: number;
  /** 备注（≤ 200 字） */
  remark: string;
}

/** 空表单：日期默认今天（CST 口径由后端兜；本机与服务器同在 CST） */
export function emptyInvoiceForm(now: Date = new Date()): InvoiceFormDraft {
  return {
    invoice_date: todayString(now),
    tax_rate: INVOICE_TAX_RATE_DEFAULT,
    remark: '',
  };
}

/**
 * 税率校验（返回人话错误；`null` = 通过）。
 *
 * 与 `validate_tax_rate` 同口径：**百分数** 0~100、最多 4 位小数。
 * 特意不把 `0.13` 猜成 13% —— 差 100 倍就是错账，宁可让人改。
 */
export function validateTaxRate(raw: number | string | null | undefined): string | null {
  if (raw === null || raw === undefined || (typeof raw === 'string' && raw.trim() === '')) {
    return `请填税率（百分数，13 = 13%；默认 ${INVOICE_TAX_RATE_DEFAULT}）`;
  }
  const value = typeof raw === 'number' ? raw : Number(raw);
  if (!Number.isFinite(value)) {
    return `税率必须是数字（收到「${String(raw)}」）`;
  }
  if (value < 0 || value > INVOICE_TAX_RATE_MAX) {
    return `税率是百分数（13 = 13%），取值 0~${INVOICE_TAX_RATE_MAX}（收到 ${value}）`;
  }
  const text = String(value);
  const dot = text.indexOf('.');
  if (dot >= 0 && text.length - dot - 1 > INVOICE_TAX_RATE_SCALE) {
    return `税率最多 ${INVOICE_TAX_RATE_SCALE} 位小数（库列是 NUMERIC(7,4)，收到 ${text}）`;
  }
  return null;
}

/** 开票日期校验（`yyyy-MM-dd`；后端吃 `NaiveDate`，格式不对直接 400） */
export function validateInvoiceDate(raw: string | null | undefined): string | null {
  const text = (raw ?? '').trim();
  if (!text) {
    return '请选开票日期（决定凭证记进哪个会计期间）';
  }
  if (!/^\d{4}-\d{2}-\d{2}$/.test(text)) {
    return `开票日期必须是 yyyy-MM-dd（收到「${text}」）`;
  }
  const [year, month, day] = text.split('-').map(Number);
  const parsed = new Date(Date.UTC(year, month - 1, day));
  if (
    parsed.getUTCFullYear() !== year ||
    parsed.getUTCMonth() !== month - 1 ||
    parsed.getUTCDate() !== day
  ) {
    return `开票日期不存在：${text}`;
  }
  return null;
}

/** 表单整体校验（先日期后税率：日期是期间闸的键） */
export function validateInvoiceForm(form: InvoiceFormDraft): string | null {
  const dateError = validateInvoiceDate(form.invoice_date);
  if (dateError) {
    return dateError;
  }
  const rateError = validateTaxRate(form.tax_rate);
  if (rateError) {
    return rateError;
  }
  if (form.remark.trim().length > INVOICE_REMARK_MAX) {
    return `备注最长 ${INVOICE_REMARK_MAX} 字（当前 ${form.remark.trim().length} 字）`;
  }
  return null;
}

/**
 * 表单 → 接口入参。
 *
 * 空日期 / 空备注**不填**（后端按「今天 / 自动备注」处理）；税率总是显式发 ——
 * 让用户在弹窗里看到的 13% 就是真正用的 13%，不靠「省了默认 13」这种隐式约定。
 */
export function buildInvoiceReq(form: InvoiceFormDraft): RebateInvoiceReq {
  const payload: RebateInvoiceReq = {
    tax_rate: Number(form.tax_rate),
  };
  const date = form.invoice_date.trim();
  if (date) {
    payload.invoice_date = date;
  }
  const remark = form.remark.trim();
  if (remark) {
    payload.remark = remark;
  }
  return payload;
}

// ==================== 试算结果展示 ====================

/** 试算结论一句话（钱在前、口径在后） */
export function invoicePreviewHeadline(preview: RebateInvoicePreview): string {
  const type = preview.invoice_type_label || '销项红字票';
  return `应返 ${formatMoney(preview.rebate_amount)}（含税）→ ${type} 价税合计 ${formatMoney(
    preview.invoice_total,
  )}（税率 ${decimalText(preview.tax_rate)}%）`;
}

/** 三列拆分的表格行（`invoice_amount` / `invoice_tax` / `invoice_total`，全为负） */
export function invoiceSplitRows(preview: RebateInvoicePreview): Array<{
  key: string;
  label: string;
  amount: string;
  meaning: string;
}> {
  return [
    {
      key: 'invoice_amount',
      label: '不含税金额',
      amount: formatMoney(preview.invoice_amount),
      meaning: '冲减主营业务收入（负数 = 红字）',
    },
    {
      key: 'invoice_tax',
      label: '税额',
      amount: formatMoney(preview.invoice_tax),
      meaning: '冲减销项税额（为 0 时凭证不出这一行）',
    },
    {
      key: 'invoice_total',
      label: '价税合计',
      amount: formatMoney(preview.invoice_total),
      meaning: '贷其他应付款-销售返利（欠客户的返利负债）',
    },
  ];
}

/** 分录方向中文（未知值原样显示，别吞成空白） */
export function voucherSideLabel(side: string | null | undefined): string {
  if (side === 'DEBIT') {
    return '借';
  }
  if (side === 'CREDIT') {
    return '贷';
  }
  return side ?? '—';
}

/** 凭证分录表格行（金额是后端给的**正值**，按方向落到借 / 贷列） */
export function invoiceVoucherRows(preview: RebateInvoicePreview): Array<{
  key: string;
  side: string;
  account: string;
  debit: string;
  credit: string;
  summary: string;
}> {
  const lines: RebateInvoiceVoucherLine[] = preview.voucher_lines ?? [];
  return lines.map((line, index) => ({
    key: `${index}-${line.account_code}`,
    side: voucherSideLabel(line.side),
    account: `${line.account_code} ${line.account_name}`.trim(),
    debit: line.side === 'DEBIT' ? formatMoney(line.amount) : '',
    credit: line.side === 'CREDIT' ? formatMoney(line.amount) : '',
    summary: line.summary,
  }));
}

/**
 * 借贷合计（核对用）。
 *
 * `balanced` 用 1 分钱容差比：金额是字符串转浮点后相加，浮点噪声不该被当成
 * 「分录不平」（后端 `InvoiceSplit::balanced` 已用 Decimal 严格相等校验过）。
 */
export function invoiceVoucherTotals(preview: RebateInvoicePreview): {
  debit: number;
  credit: number;
  balanced: boolean;
} {
  let debit = 0;
  let credit = 0;
  for (const line of preview.voucher_lines ?? []) {
    if (line.side === 'DEBIT') {
      debit += toNumber(line.amount);
    } else if (line.side === 'CREDIT') {
      credit += toNumber(line.amount);
    }
  }
  return { debit, credit, balanced: Math.abs(debit - credit) < 0.005 };
}

/** 拦下来的原因（后端 `blockers` 原文；空串行丢掉） */
export function invoiceBlockers(preview: RebateInvoicePreview): string[] {
  return (preview.blockers ?? []).filter((item) => item.trim().length > 0);
}

/** 不拦的提醒（后端 `warnings` 原文） */
export function invoiceWarnings(preview: RebateInvoicePreview): string[] {
  return (preview.warnings ?? []).filter((item) => item.trim().length > 0);
}

/** 能否确认开票（以**试算结果**为准，不是以行的状态为准） */
export function canConfirmInvoice(preview: RebateInvoicePreview | null): boolean {
  return Boolean(preview && preview.can_generate);
}

/** 确认按钮文案（带上要冲的金额，避免手快手错） */
export function invoiceConfirmText(preview: RebateInvoicePreview | null): string {
  if (!preview) {
    return '确认开票';
  }
  if (!preview.can_generate) {
    return '不能开票（见上方原因）';
  }
  return `确认开票（价税合计 ${formatMoney(preview.invoice_total)}）`;
}

/** 已开票回显（预览里 `existing_invoice` 非空时；`DRAFT` 为 `null`） */
export function invoiceExistingText(preview: RebateInvoicePreview): string {
  const invoice = preview.existing_invoice;
  if (!invoice) {
    return '';
  }
  const voucher = invoice.voucher_no ? `，凭证 ${invoice.voucher_no}` : '';
  return `该结算单已开过红字发票 ${invoice.invoice_no}（${invoice.invoice_date} 开出，价税合计 ${formatMoney(
    invoice.invoice_total,
  )}${voucher}）`;
}

/** 开票成功后的提示（票号 / 凭证号 / 结算单新状态三件事都要说） */
export function invoiceResultText(res: RebateInvoiceRow): string {
  const voucher = res.voucher_no ? `，凭证 ${res.voucher_no}` : '';
  return `已开红字发票 ${res.invoice_no}（价税合计 ${formatMoney(
    res.invoice_total,
  )}${voucher}）；结算单 ${res.settlement_no} 转为「${res.settlement_status_label || res.settlement_status}」`;
}

/**
 * 错误码 → 额外出路（**在全局人话之下再补一句「现在该点哪」**）。
 *
 * `constants/error-messages.ts` 已把这三条码翻成人话（含动作），这里只补
 * 「在这个弹窗里具体改哪个字段」—— 同一个码在不同页面出路不同，页面比码表更懂上下文。
 */
export function invoiceErrorGuidance(code: number | null | undefined): string {
  switch (code) {
    case 66093:
      return '出路：这张单已经不是草稿了。点「刷新」看最新状态 —— 已开过票的走红冲 / 调账，已作废的重新算一张草稿单。';
    case 66094:
      return '出路：应返金额为 0 或负数，开不出红字票。确认这期确实没有返利请作废该单；金额不对请先「重算」。';
    case 66095:
      return '出路：把上方「开票日期」改到未结账的月份（凭证按开票日入账），或先反结账再开票。';
    default:
      return '';
  }
}

/**
 * 行级「先知道结论」的提示：**打开弹窗就能看见**该码的码表人话 + 本页出路。
 *
 * 为什么要有：`rebate_amount = 0` 的草稿根本走不到 POST（确认按钮禁用），
 * 但用户仍然需要知道「为什么开不了、该改什么」—— 那正是 `66094` 这条翻译存在的理由。
 * 期间闸（`66095`）依赖用户选的开票日，只能由后端试算 `blockers` 回答，这里不猜。
 */
export function invoiceRowAdvice(
  row: Pick<RebateSettlementRow, 'status' | 'rebate_amount'>,
): { code: number; message: string; guidance: string } | null {
  if (canInvoice(row)) {
    return null;
  }
  const code = row.status === 'DRAFT' ? 66094 : 66093;
  return {
    code,
    message: resolveErrorMessage({ code }).message,
    guidance: invoiceErrorGuidance(code),
  };
}

/** 小数文案（`13.0000` → `13`、`13.5` → `13.5`） */
export function decimalText(value: unknown): string {
  const num = toNumber(value as number | string | null | undefined);
  if (!Number.isFinite(num)) {
    return String(value ?? '');
  }
  return String(Number(num.toFixed(INVOICE_TAX_RATE_SCALE)));
}
