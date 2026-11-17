/**
 * 销售返利「开红字发票」纯逻辑单测（M3-19F 前端 / 后端 M3-17）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * # 为什么测 `.ts` 不测 `.vue`
 *
 * 「哪些单能开票」「税率能不能填 13.12345」「开票日 2026-02-30 收不收」
 * 「三列怎么落到借 / 贷」「后端 66094 要提示哪句出路」全在 `invoice-logic.ts` 里，
 * `.vue` 只渲染。这些一旦漂移 `vue-tsc` 抓不到（编译期两边都是 `string | number`），
 * 而线上表现是**点下去才 400** 或者**开出错误的票**。
 *
 * # 源码即契约（三条，直接读后端文件断言）
 *
 * 1. 权限码 `business:erp:rebate-settlement:invoice` 逐字一致（`rebate_invoice_handler.rs` +
 *    `api/business/erp/rebate.ts` 的 `REBATE_PERMISSIONS`）；
 * 2. **`invoice-preview` 是 POST**（`rebate_invoice_handler.rs` 是 `#[nik::post]`）——
 *    卡面写的 `GET` 是笔误，前端要是照着写 GET 会 405，这里钉死；
 * 3. 票种恒 `OUT_RED`、错误码 66093/66094/66095 在 `error_code.rs` 里真实存在。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';

import {
  INVOICE_REMARK_MAX,
  INVOICE_TAX_RATE_DEFAULT,
  INVOICE_TAX_RATE_MAX,
  buildInvoiceReq,
  canConfirmInvoice,
  canInvoice,
  decimalText,
  emptyInvoiceForm,
  invoiceBlockers,
  invoiceConfirmText,
  invoiceErrorGuidance,
  invoiceExistingText,
  invoiceGateTip,
  invoicePreviewHeadline,
  invoiceResultText,
  invoiceRowAdvice,
  invoiceSplitRows,
  invoiceVoucherRows,
  invoiceVoucherTotals,
  invoiceWarnings,
  showInvoiceAction,
  validateInvoiceDate,
  validateInvoiceForm,
  validateTaxRate,
  voucherSideLabel,
} from '@/views/business/erp/rebate/invoice-logic';
import type {
  RebateInvoicePreview,
  RebateInvoiceRow,
  RebateSettlementRow,
} from '@/api/business/erp/rebate';

const HERE = dirname(fileURLToPath(import.meta.url));
/** `frontend/tests/unit` → 仓根 */
const REPO_ROOT = resolve(HERE, '..', '..', '..');

function read(relative: string): string {
  return readFileSync(resolve(REPO_ROOT, relative), 'utf8');
}

const INVOICE_DTO = read('backend/app/src/business/erp/sales/rebate_invoice_dto.rs');
const INVOICE_HANDLER = read('backend/app/src/business/erp/sales/rebate_invoice_handler.rs');
const FRONTEND_API = read('frontend/src/api/business/erp/rebate.ts');
const ERROR_CODE_RS = read('backend/crates/cloudpivot-kernel/src/shared/error_code.rs');

/** 一行结算单（只填判据用得到的字段） */
function settleRow(patch: Partial<RebateSettlementRow> = {}): RebateSettlementRow {
  return {
    id: '0192f0aa-0000-7000-8000-000000000001',
    settlement_no: 'RB-20261117-000001',
    customer_id: '0192f0aa-0000-7000-8000-0000000000c1',
    customer_code: 'C001',
    customer_name: '客户一',
    settle_period: 'MONTH',
    settle_period_label: '月度',
    period_code: '2026-11',
    period_from: '2026-11-01',
    period_to: '2026-12-01',
    period_last_day: '2026-11-30',
    policy_id: '0192f0aa-0000-7000-8000-0000000000p1',
    policy_no: 'BR-20261101-000001',
    policy_name: '11 月返利',
    rule_type: 'BY_AMOUNT',
    rule_type_label: '按额',
    rate_unit: 'PERCENT',
    rate_unit_label: '百分比',
    policy_effective_from: '2026-01-01',
    policy_effective_to: null,
    policy_tiers: [],
    base: {
      order_count: 1,
      order_amount: '100000.00',
      order_qty: '10.0000',
      return_count: 0,
      return_amount: '0.00',
      return_qty: '0.0000',
      net_amount: '100000.00',
      net_qty: '10.0000',
    },
    tier_seq: 1,
    tier_from: '0',
    tier_to: null,
    rate: '1',
    rebate_amount: '1000.00',
    status: 'DRAFT',
    status_label: '草稿',
    remark: null,
    created_at: '2026-11-17T02:00:00Z',
    updated_at: '2026-11-17T02:00:00Z',
    invoice_id: null,
    invoice_no: null,
    settled_at: null,
    ...patch,
  };
}

/** 一张已开的红字发票（后端 `RebateInvoiceRes`） */
function invoiceRow(patch: Partial<RebateInvoiceRow> = {}): RebateInvoiceRow {
  return {
    settlement_id: '0192f0aa-0000-7000-8000-000000000001',
    settlement_no: 'RB-20261117-000001',
    settlement_status: 'SETTLED',
    settlement_status_label: '已开红字发票',
    customer_id: '0192f0aa-0000-7000-8000-0000000000c1',
    customer_code: 'C001',
    customer_name: '客户一',
    settle_period: 'MONTH',
    period_code: '2026-11',
    rebate_amount: '1000.00',
    invoice_id: '0192f0aa-0000-7000-8000-0000000000i1',
    invoice_no: 'IV-20261117-000001',
    invoice_type: 'OUT_RED',
    invoice_type_label: '销项红字票',
    invoice_date: '2026-11-17',
    invoice_amount: '-884.96',
    invoice_tax: '-115.04',
    invoice_total: '-1000.00',
    tax_rate: '13.0000',
    currency_code: 'CNY',
    invoice_status: 'ISSUED',
    voucher_id: '0192f0aa-0000-7000-8000-0000000000v1',
    voucher_no: 'PZ-20261117-000001',
    settled_at: '2026-11-17T03:00:00Z',
    ...patch,
  };
}

/** 一份票面试算结果（后端 `RebateInvoicePreviewRes`；金额与 1000 @13% 的真实拆分一致） */
function preview(patch: Partial<RebateInvoicePreview> = {}): RebateInvoicePreview {
  return {
    settlement_id: '0192f0aa-0000-7000-8000-000000000001',
    settlement_no: 'RB-20261117-000001',
    settlement_status: 'DRAFT',
    settlement_status_label: '草稿',
    customer_id: '0192f0aa-0000-7000-8000-0000000000c1',
    customer_code: 'C001',
    customer_name: '客户一',
    settle_period: 'MONTH',
    period_code: '2026-11',
    rebate_amount: '1000.00',
    tax_rate: '13.0000',
    invoice_type: 'OUT_RED',
    invoice_type_label: '销项红字票',
    invoice_date: '2026-11-17',
    currency_code: 'CNY',
    invoice_amount: '-884.96',
    invoice_tax: '-115.04',
    invoice_total: '-1000.00',
    can_generate: true,
    blockers: [],
    warnings: [],
    voucher_lines: [
      {
        side: 'DEBIT',
        account_code: '5001',
        account_name: '主营业务收入',
        amount: '884.96',
        summary: '冲减主营业务收入',
      },
      {
        side: 'DEBIT',
        account_code: '2221.02',
        account_name: '应交税费-销项税',
        amount: '115.04',
        summary: '冲减销项税',
      },
      {
        side: 'CREDIT',
        account_code: '2241.05',
        account_name: '其他应付款-销售返利',
        amount: '1000.00',
        summary: '应付客户返利',
      },
    ],
    existing_invoice: null,
    ...patch,
  };
}

// ==================== 源码即契约 ====================

test('权限码与后端 / 前端逐字一致（V0440 种的 business:erp:rebate-settlement:invoice）', () => {
  assert.match(INVOICE_HANDLER, /pub const PERM_INVOICE: &str = "business:erp:rebate-settlement:invoice"/);
  assert.match(FRONTEND_API, /settleInvoice: 'business:erp:rebate-settlement:invoice'/);
  // 开票是写接口，不能复用列表码（后端单测同一条断言）
  assert.doesNotMatch(INVOICE_HANDLER, /PERM_INVOICE: &str = "business:erp:rebate-settlement:list"/);
});

test('票面试算是 POST（卡面写 GET 是笔误；前端照 GET 写会 405）', () => {
  assert.match(
    INVOICE_HANDLER,
    /#\[nik::post\("\/business\/erp\/rebate-settlement\/\{id\}\/invoice-preview"\)\]/,
    '后端 preview 必须是 #[nik::post]',
  );
  assert.doesNotMatch(
    INVOICE_HANDLER,
    /#\[nik::get\("\/business\/erp\/rebate-settlement\/\{id\}\/invoice-preview"\)\]/,
    '后端 preview 没有 GET 版本',
  );
  assert.match(
    FRONTEND_API,
    /return post<RebateInvoicePreview>\(`\$\{SETTLE_BASE\}\/\$\{id\}\/invoice-preview`, payload\)/,
    '前端 preview 必须走 post()',
  );
  assert.match(
    INVOICE_HANDLER,
    /#\[nik::post\("\/business\/erp\/rebate-settlement\/\{id\}\/invoice"\)\]/,
  );
});

test('票种 / 默认税率 / 备注上限与后端常量一致', () => {
  assert.match(INVOICE_DTO, /pub const INVOICE_TYPE_OUT_RED: &str = "OUT_RED"/);
  assert.match(INVOICE_DTO, /pub const DEFAULT_TAX_RATE_PERCENT: i32 = 13/);
  assert.match(INVOICE_DTO, /pub const TAX_RATE_MAX_PERCENT: i32 = 100/);
  assert.match(INVOICE_DTO, /pub const MAX_REMARK_LEN: usize = 200/);
  assert.equal(INVOICE_TAX_RATE_DEFAULT, 13);
  assert.equal(INVOICE_TAX_RATE_MAX, 100);
  assert.equal(INVOICE_REMARK_MAX, 200);
});

test('三道闸的错误码在 error_code.rs 里真实存在（66093 / 66094 / 66095）', () => {
  assert.match(ERROR_CODE_RS, /ERP_REBATE_INVOICE_STATE: i32 = 66093/);
  assert.match(ERROR_CODE_RS, /ERP_REBATE_INVOICE_AMOUNT: i32 = 66094/);
  assert.match(ERROR_CODE_RS, /ERP_REBATE_INVOICE_PERIOD_CLOSED: i32 = 66095/);
  // 三条都有出路文案，不能返回空串
  for (const code of [66093, 66094, 66095]) {
    assert.ok(invoiceErrorGuidance(code).startsWith('出路：'), `${code} 必须有出路文案`);
  }
});

// ==================== 按钮显隐 / 可开判据 ====================

test('只有草稿行渲染「开票」入口（已结算看票号、已作废无入口）', () => {
  assert.equal(showInvoiceAction(settleRow({ status: 'DRAFT' })), true);
  assert.equal(showInvoiceAction(settleRow({ status: 'SETTLED' })), false);
  assert.equal(showInvoiceAction(settleRow({ status: 'CANCELLED' })), false);
});

test('可开 = 草稿 且 应返 > 0（0 / 负数 / 非草稿都不可开）', () => {
  assert.equal(canInvoice(settleRow({ status: 'DRAFT', rebate_amount: '1000.00' })), true);
  assert.equal(canInvoice(settleRow({ status: 'DRAFT', rebate_amount: '0.01' })), true);
  assert.equal(canInvoice(settleRow({ status: 'DRAFT', rebate_amount: '0.00' })), false);
  assert.equal(canInvoice(settleRow({ status: 'DRAFT', rebate_amount: 0 })), false);
  assert.equal(canInvoice(settleRow({ status: 'DRAFT', rebate_amount: '-1.00' })), false);
  assert.equal(canInvoice(settleRow({ status: 'SETTLED', rebate_amount: '1000.00' })), false);
  assert.equal(canInvoice(settleRow({ status: 'CANCELLED', rebate_amount: '1000.00' })), false);
});

test('tooltip 三种口径：可用 / 已开过 / 金额为 0', () => {
  const ok = invoiceGateTip(settleRow());
  assert.match(ok, /票面试算/);

  const settled = invoiceGateTip(
    settleRow({ status: 'SETTLED', invoice_no: 'IV-20261117-000001' }),
  );
  assert.match(settled, /IV-20261117-000001/);
  assert.match(settled, /红冲/);

  const cancelled = invoiceGateTip(settleRow({ status: 'CANCELLED' }));
  assert.match(cancelled, /作废/);
  assert.match(cancelled, /重新算/);

  const zero = invoiceGateTip(settleRow({ rebate_amount: '0.00' }));
  assert.match(zero, /66094/);
  assert.match(zero, /作废|重算/);
});

// ==================== 表单校验 ====================

test('税率：百分数 0~100、最多 4 位小数（0.13 不会被猜成 13%）', () => {
  assert.equal(validateTaxRate(13), null);
  assert.equal(validateTaxRate('13'), null);
  assert.equal(validateTaxRate(0), null);
  assert.equal(validateTaxRate(100), null);
  assert.equal(validateTaxRate('13.0000'), null);
  assert.equal(validateTaxRate(13.1234), null);

  assert.match(validateTaxRate(100.01) ?? '', /0~100/);
  assert.match(validateTaxRate(-1) ?? '', /0~100/);
  assert.match(validateTaxRate(13.12345) ?? '', /4 位小数/);
  assert.match(validateTaxRate('') ?? '', /请填税率/);
  assert.match(validateTaxRate(null) ?? '', /请填税率/);
  assert.match(validateTaxRate('abc') ?? '', /必须是数字/);
  // 0.13 是合法值（0.13%），但绝不会被当成 13% —— 提示里给出的是它自己
  assert.equal(validateTaxRate(0.13), null);
});

test('开票日期：yyyy-MM-dd 且必须是真实存在的日（2026-02-30 拒）', () => {
  assert.equal(validateInvoiceDate('2026-11-17'), null);
  assert.equal(validateInvoiceDate('2026-02-28'), null);
  assert.equal(validateInvoiceDate('2026-12-01'), null);
  assert.match(validateInvoiceDate('') ?? '', /请选开票日期/);
  assert.match(validateInvoiceDate('2026/11/17') ?? '', /yyyy-MM-dd/);
  assert.match(validateInvoiceDate('2026-13-01') ?? '', /yyyy-MM-dd|不存在/);
  assert.match(validateInvoiceDate('2026-02-30') ?? '', /不存在/);
});

test('表单整体校验：日期先于税率，备注超长也拦', () => {
  assert.equal(validateInvoiceForm(emptyInvoiceForm(new Date('2026-11-17T02:00:00Z'))), null);
  assert.match(
    validateInvoiceForm({ invoice_date: '', tax_rate: 13, remark: '' }) ?? '',
    /请选开票日期/,
  );
  assert.match(
    validateInvoiceForm({ invoice_date: '2026-11-17', tax_rate: 101, remark: '' }) ?? '',
    /0~100/,
  );
  const longRemark = '字'.repeat(INVOICE_REMARK_MAX + 1);
  assert.match(
    validateInvoiceForm({ invoice_date: '2026-11-17', tax_rate: 13, remark: longRemark }) ?? '',
    /备注最长 200 字/,
  );
});

test('空表单：开票日 = 今天（CST）、税率 = 13、备注空', () => {
  const form = emptyInvoiceForm(new Date('2026-11-17T02:00:00Z'));
  assert.equal(form.invoice_date, '2026-11-17');
  assert.equal(form.tax_rate, 13);
  assert.equal(form.remark, '');
});

test('入参：税率总是显式发、日期/备注为空则不发', () => {
  assert.deepEqual(buildInvoiceReq({ invoice_date: '2026-11-17', tax_rate: 13, remark: '' }), {
    tax_rate: 13,
    invoice_date: '2026-11-17',
  });
  assert.deepEqual(buildInvoiceReq({ invoice_date: '  ', tax_rate: 6, remark: '  红冲 11 月  ' }), {
    tax_rate: 6,
    remark: '红冲 11 月',
  });
  // 税率不会因为「等于默认值」就被省掉（省了后端也当 13，但显式更可审计）
  assert.equal(buildInvoiceReq({ invoice_date: '2026-11-17', tax_rate: 13, remark: '' }).tax_rate, 13);
});

// ==================== 试算结果展示 ====================

test('三列拆分：负数原样展示，不再补负号', () => {
  const rows = invoiceSplitRows(preview());
  assert.equal(rows.length, 3);
  assert.deepEqual(
    rows.map((row) => row.label),
    ['不含税金额', '税额', '价税合计'],
  );
  assert.equal(rows[0].amount, '-884.96');
  assert.equal(rows[1].amount, '-115.04');
  assert.equal(rows[2].amount, '-1,000.00');
  // 千分位（财务读得懂）
  assert.equal(rows[2].amount.includes(','), true);
});

test('结论一句话：含税应返 → 价税合计 + 税率', () => {
  const text = invoicePreviewHeadline(preview());
  assert.match(text, /应返 1,000\.00/);
  assert.match(text, /-1,000\.00/);
  assert.match(text, /13%/);
  assert.match(text, /销项红字票/);
});

test('凭证分录：借 / 贷分列，合计平衡；税额为 0 时只有两行', () => {
  const rows = invoiceVoucherRows(preview());
  assert.equal(rows.length, 3);
  assert.deepEqual(
    rows.map((row) => row.side),
    ['借', '借', '贷'],
  );
  assert.equal(rows[0].debit, '884.96');
  assert.equal(rows[0].credit, '');
  assert.equal(rows[2].debit, '');
  assert.equal(rows[2].credit, '1,000.00');
  assert.match(rows[2].account, /2241\.05/);
  assert.match(rows[2].account, /其他应付款-销售返利/);

  const totals = invoiceVoucherTotals(preview());
  assert.equal(totals.balanced, true);
  assert.ok(Math.abs(totals.debit - 1000) < 0.005);
  assert.ok(Math.abs(totals.credit - 1000) < 0.005);

  // 0% 税率：后端不出销项税那一行
  const zeroTax = preview({
    invoice_tax: '0',
    invoice_amount: '-1000.00',
    voucher_lines: [
      {
        side: 'DEBIT',
        account_code: '5001',
        account_name: '主营业务收入',
        amount: '1000.00',
        summary: '冲减主营业务收入',
      },
      {
        side: 'CREDIT',
        account_code: '2241.05',
        account_name: '其他应付款-销售返利',
        amount: '1000.00',
        summary: '应付客户返利',
      },
    ],
  });
  assert.equal(invoiceVoucherRows(zeroTax).length, 2);
  assert.equal(invoiceVoucherTotals(zeroTax).balanced, true);
});

test('分录方向：未知值原样显示（不吞成空白），借贷不平能识别', () => {
  assert.equal(voucherSideLabel('DEBIT'), '借');
  assert.equal(voucherSideLabel('CREDIT'), '贷');
  assert.equal(voucherSideLabel('FOO'), 'FOO');
  assert.equal(voucherSideLabel(null), '—');

  const unbalanced = preview({
    voucher_lines: [
      {
        side: 'DEBIT',
        account_code: '5001',
        account_name: '主营业务收入',
        amount: '900.00',
        summary: '冲减主营业务收入',
      },
      {
        side: 'CREDIT',
        account_code: '2241.05',
        account_name: '其他应付款-销售返利',
        amount: '1000.00',
        summary: '应付客户返利',
      },
    ],
  });
  assert.equal(invoiceVoucherTotals(unbalanced).balanced, false);
});

test('拦在哪：blockers / warnings 原样透出，空串行丢掉', () => {
  const blocked = preview({
    can_generate: false,
    blockers: ['会计期间 2026-10 已结账：请把开票日期改到未结账期间，或先反结账', '   '],
    warnings: ['本次按 6% 拆分（默认 13%）：与销售发票税率不一致时请确认口径', ''],
  });
  assert.deepEqual(invoiceBlockers(blocked), [
    '会计期间 2026-10 已结账：请把开票日期改到未结账期间，或先反结账',
  ]);
  assert.deepEqual(invoiceWarnings(blocked), [
    '本次按 6% 拆分（默认 13%）：与销售发票税率不一致时请确认口径',
  ]);
  assert.equal(canConfirmInvoice(blocked), false);
  assert.equal(canConfirmInvoice(preview()), true);
  assert.equal(canConfirmInvoice(null), false);
});

test('确认按钮文案：带价税合计 / 不能开时明说', () => {
  assert.match(invoiceConfirmText(preview()), /确认开票（价税合计 -1,000\.00）/);
  assert.match(invoiceConfirmText(preview({ can_generate: false })), /不能开票/);
  assert.equal(invoiceConfirmText(null), '确认开票');
});

test('已开票回显：无票返回空串，有票带票号 / 凭证号', () => {
  assert.equal(invoiceExistingText(preview()), '');
  const settled = preview({ settlement_status: 'SETTLED', existing_invoice: invoiceRow() });
  const text = invoiceExistingText(settled);
  assert.match(text, /IV-20261117-000001/);
  assert.match(text, /PZ-20261117-000001/);
  assert.match(text, /-1,000\.00/);
});

test('成功提示：票号 / 凭证号 / 结算单新状态三件事都在', () => {
  const text = invoiceResultText(invoiceRow());
  assert.match(text, /IV-20261117-000001/);
  assert.match(text, /PZ-20261117-000001/);
  assert.match(text, /已开红字发票/);
  assert.match(text, /已开红字发票」|已开红字发票/);
  assert.match(text, /RB-20261117-000001/);

  // 凭证号缺失时不出现「凭证 undefined」
  const noVoucher = invoiceResultText(invoiceRow({ voucher_no: null, voucher_id: null }));
  assert.doesNotMatch(noVoucher, /undefined/);
  assert.doesNotMatch(noVoucher, /凭证/);
});

test('税率文案：13.0000 → 13、13.5 → 13.5', () => {
  assert.equal(decimalText('13.0000'), '13');
  assert.equal(decimalText('13.5000'), '13.5');
  assert.equal(decimalText(6), '6');
});

test('行级结论：能开时无提示；金额 0 给 66094 码表人话；非草稿给 66093', () => {
  assert.equal(invoiceRowAdvice(settleRow()), null);

  const zero = invoiceRowAdvice(settleRow({ rebate_amount: '0.00' }));
  assert.ok(zero, '金额 0 必须有提示');
  assert.equal(zero.code, 66094);
  assert.ok(zero.message.length > 10, '66094 必须是码表人话，不是裸码');
  assert.doesNotMatch(zero.message, /^66094$/);
  assert.match(zero.guidance, /^出路：/);

  const settled = invoiceRowAdvice(settleRow({ status: 'SETTLED' }));
  assert.ok(settled);
  assert.equal(settled.code, 66093);
  assert.match(settled.guidance, /^出路：/);
});
