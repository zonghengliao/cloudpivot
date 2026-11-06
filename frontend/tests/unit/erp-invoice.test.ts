/**
 * `invoice-logic.ts` 单测（SO-21 销售发票页的纯逻辑）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * 为什么测 `.ts` 不测 `.vue`：用例跑在没有 DOM 的 node 里。下面这些一旦漂移
 * `vue-tsc` 抓不到（编译期两边都是 `string`），但线上会**算错钱或点不动按钮**：
 *
 * | 断言 | 后端出处 | 漂移后果 |
 * |------|----------|----------|
 * | 状态域 4 值 ≠ 应收 / 收款那套 | `invoice::STATUSES` / `ck_biz_invoice_status` | 传 `OPEN` / `CANCELLED` → 400 |
 * | 边表 4 条（无 POSTED→VOID、无回 DRAFT） | `state_machine::INVOICE_EDGES` | 按钮出现在不该出现的状态上 → 409 |
 * | 两条作废边都要原因 | `INVOICE_REASON_EDGES` | 空原因被后端 400（用户白填一遍单） |
 * | 额度排除 VOID | `invoice::invoiced_total`：`status <> 'VOID'` | 作废后额度不恢复 → 永远开不出第二张 |
 * | 不含税 + 税额 恒等于合计 | `invoice::split_tax` | 预览与后端差 1 分，用户以为填错 |
 * | 作废原因按**字符**数 | `invoice::REASON_MIN_CHARS = 4` | 中文 2 字被当 6 字节放过 |
 * | 空筛选不发空串 | `InvoicePageQuery::validate` | `?status=` 查 0 行、`?customer_id=` 400 |
 *
 * 最后一段直接读后端源码做「逐字契约」断言：状态域 / 边表 / 原因长度 / 票种 / 科目常量
 * 改了后端而没改前端，这里先红（比人眼比对可靠）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import type { Invoice } from '@/api/business/erp';
import {
  INVOICE_COLUMNS,
  INVOICE_DEFAULT_TAX_RATE,
  INVOICE_EDGES,
  INVOICE_LOCKED,
  INVOICE_PAGE_SIZE,
  INVOICE_PERMS,
  INVOICE_REASON_EDGES,
  INVOICE_REASON_MAX,
  INVOICE_REASON_MIN,
  INVOICE_STATUSES,
  INVOICE_STATUS_LABELS,
  INVOICE_STATUS_OPTIONS,
  INVOICE_TYPES,
  INVOICE_TYPE_LABELS,
  allowedNextStatuses,
  buildInvoiceQuery,
  canArchive,
  canIssue,
  canTransition,
  canVoid,
  edgeNeedsReason,
  emptyInvoiceFilter,
  invoiceFormError,
  invoiceHeadroomCents,
  invoiceReasonError,
  invoiceSourceText,
  invoiceStatusLabel,
  invoiceStatusTheme,
  invoiceTypeLabel,
  invoicedTotalCents,
  isInvoiceFiltered,
  nextActionHint,
  receivableOptionLabel,
  splitTaxCents,
  taxRateText,
  voidBlockedReason,
  voidImpactText,
  type InvoiceFormState,
} from '@/views/business/erp/invoice/invoice-logic';

/** 一条最小可用的发票行（金额默认 1130 = 1000 不含税 + 130 税） */
function row(overrides: Partial<Invoice> = {}): Invoice {
  return {
    id: 'inv-1',
    invoice_no: 'IV-20261106-000001',
    invoice_type: 'OUT',
    invoice_type_label: '销项正票',
    invoice_code: null,
    customer_id: 'cust-1',
    customer_name: '北京测试客户',
    invoice_date: '2026-11-06',
    amount: '1000.0000',
    tax_amount: '130.0000',
    total_with_tax: '1130.0000',
    tax_rate: '0.1300',
    status: 'DRAFT',
    status_label: '草稿',
    receivable_id: 'ar-1',
    source_type: 'SALES_OUT',
    source_no: 'OUT-20261106-000001',
    voucher_id: null,
    remark: null,
    created_at: null,
    updated_at: null,
    ...overrides,
  };
}

function form(overrides: Partial<InvoiceFormState> = {}): InvoiceFormState {
  return {
    receivableId: 'ar-1',
    invoiceDate: '2026-11-06',
    total: 1130,
    taxRate: INVOICE_DEFAULT_TAX_RATE,
    invoiceNo: '',
    invoiceCode: '',
    remark: '',
    ...overrides,
  };
}

// ==================== 域值 / 边表 ====================

test('状态域 = DRAFT/ISSUED/POSTED/VOID（不含应收的 OPEN、收款的 CANCELLED）', () => {
  assert.deepEqual(INVOICE_STATUSES, ['DRAFT', 'ISSUED', 'POSTED', 'VOID']);
  assert.equal(INVOICE_STATUSES.includes('OPEN' as never), false, 'OPEN 是应收的域值');
  assert.equal(INVOICE_STATUSES.includes('CANCELLED' as never), false, 'CANCELLED 是收款/应收的域值，发票用 VOID');
  assert.equal(INVOICE_STATUSES.includes('PARTIAL' as never), false);
  for (const status of INVOICE_STATUSES) {
    assert.ok(INVOICE_STATUS_LABELS[status], `${status} 缺中文标签`);
    assert.ok(invoiceStatusTheme(status));
  }
  // 未知状态不许被当成合法态
  assert.equal(invoiceStatusTheme('OPEN'), 'default');
  assert.equal(invoiceStatusLabel({ status: 'OPEN', status_label: '' }), 'OPEN');
});

test('边表 4 条：没有 POSTED→VOID、没有任何边回到 DRAFT', () => {
  assert.deepEqual(INVOICE_EDGES, [
    ['DRAFT', 'ISSUED'],
    ['ISSUED', 'POSTED'],
    ['DRAFT', 'VOID'],
    ['ISSUED', 'VOID'],
  ]);
  for (const [from, to] of INVOICE_EDGES) {
    assert.ok(INVOICE_STATUSES.includes(from), `边起点 ${from} 不在状态域`);
    assert.ok(INVOICE_STATUSES.includes(to), `边终点 ${to} 不在状态域`);
    assert.notEqual(to, 'DRAFT', '没有任何边回到草稿（发票一旦取号不能原地重开）');
  }
  assert.equal(canTransition('POSTED', 'VOID'), false, '已归档是终态');
  assert.equal(canTransition('VOID', 'ISSUED'), false);
  assert.equal(canTransition('DRAFT', 'POSTED'), false, '不许跳过开票直接归档（后端 409）');
  assert.deepEqual(allowedNextStatuses('DRAFT'), ['ISSUED', 'VOID']);
  assert.deepEqual(allowedNextStatuses('ISSUED'), ['POSTED', 'VOID']);
  assert.deepEqual(allowedNextStatuses('POSTED'), []);
  assert.deepEqual(allowedNextStatuses('VOID'), []);
  assert.deepEqual(INVOICE_LOCKED, ['ISSUED', 'POSTED', 'VOID']);
});

test('两条作废边都要原因；开票 / 归档不要', () => {
  assert.deepEqual(INVOICE_REASON_EDGES, [
    ['DRAFT', 'VOID'],
    ['ISSUED', 'VOID'],
  ]);
  for (const [from, to] of INVOICE_REASON_EDGES) {
    assert.ok(
      INVOICE_EDGES.some(([f, t]) => f === from && t === to),
      `reason 边 ${from}→${to} 不在合法边表里`,
    );
    assert.equal(edgeNeedsReason(from, to), true);
  }
  assert.equal(edgeNeedsReason('DRAFT', 'ISSUED'), false);
  assert.equal(edgeNeedsReason('ISSUED', 'POSTED'), false);
});

test('按钮可用性：开票 / 归档 / 作废按状态互斥', () => {
  assert.equal(canIssue({ status: 'DRAFT' }), true);
  assert.equal(canIssue({ status: 'ISSUED' }), false);
  assert.equal(canArchive({ status: 'ISSUED' }), true);
  assert.equal(canArchive({ status: 'DRAFT' }), false);
  assert.equal(canVoid({ status: 'DRAFT' }), true);
  assert.equal(canVoid({ status: 'ISSUED' }), true);
  assert.equal(canVoid({ status: 'POSTED' }), false);
  assert.equal(canVoid({ status: 'VOID' }), false);
  // 置灰原因要说出为什么，别只显示「不可用」
  assert.equal(voidBlockedReason({ status: 'DRAFT' }), '');
  assert.equal(voidBlockedReason({ status: 'ISSUED' }), '');
  assert.match(voidBlockedReason({ status: 'POSTED' }), /归档|红字票/);
  assert.match(voidBlockedReason({ status: 'VOID' }), /已作废/);
});

test('作废影响提示：ISSUED 来的必须提示红冲凭证 + 撤回应收发票号', () => {
  const issued = voidImpactText({ status: 'ISSUED' });
  assert.match(issued, /红字凭证/);
  assert.match(issued, /发票号/);
  const draft = voidImpactText({ status: 'DRAFT' });
  assert.match(draft, /草稿/);
  assert.equal(/红字凭证/.test(draft), false, '草稿作废没有凭证可冲，别吓用户');
});

test('下一步提示覆盖 4 态且提到凭证 / 红字票', () => {
  assert.match(nextActionHint({ status: 'DRAFT' }), /开票|1122/);
  assert.match(nextActionHint({ status: 'ISSUED' }), /归档|作废/);
  assert.match(nextActionHint({ status: 'POSTED' }), /终态|红字票/);
  assert.match(nextActionHint({ status: 'VOID' }), /终态/);
  assert.match(nextActionHint({ status: 'NOPE' }), /未知状态/);
});

// ==================== 票种 ====================

test('票种域与中文（OUT_RED 由退货流程生成，手工建会被 400）', () => {
  assert.deepEqual(INVOICE_TYPES, ['OUT', 'OUT_RED']);
  assert.equal(INVOICE_TYPE_LABELS.OUT, '销项正票');
  assert.equal(INVOICE_TYPE_LABELS.OUT_RED, '销项红字票');
  assert.equal(
    invoiceTypeLabel({ invoice_type: 'OUT', invoice_type_label: '销项正票' }),
    '销项正票',
  );
  assert.equal(invoiceTypeLabel({ invoice_type: 'OUT_RED', invoice_type_label: '' }), '销项红字票');
  assert.equal(invoiceTypeLabel({ invoice_type: 'IN', invoice_type_label: '' }), 'IN');
});

test('来源文案：单号 + 中文类型；手工挂账不重复成「MANUAL（MANUAL）」', () => {
  assert.equal(invoiceSourceText({ source_type: 'SALES_OUT', source_no: 'OUT-20261106-000001' }), 'OUT-20261106-000001（销售出库）');
  assert.equal(invoiceSourceText({ source_type: 'MANUAL', source_no: 'MANUAL' }), '手工挂账');
  assert.equal(invoiceSourceText({ source_type: 'MANUAL', source_no: '' }), '手工挂账');
  assert.equal(invoiceSourceText({ source_type: null, source_no: null }), '—');
  assert.equal(invoiceSourceText({ source_type: 'SHIPMENT', source_no: 'SH-1' }), 'SH-1（发货单）');
});

// ==================== 金额 ====================

test('税额拆分：先定不含税，税额 = 合计 − 不含税（两者相加恒等于合计）', () => {
  assert.deepEqual(splitTaxCents(113000, 0.13), { amountCents: 100000, taxCents: 13000 });
  assert.deepEqual(splitTaxCents(22600, 0.13), { amountCents: 20000, taxCents: 2600 });
  assert.deepEqual(splitTaxCents(0, 0.13), { amountCents: 0, taxCents: 0 });
  // 0 税率：全额是不含税，税额 0
  assert.deepEqual(splitTaxCents(10000, 0), { amountCents: 10000, taxCents: 0 });
  // 红字票（负合计）对称
  assert.deepEqual(splitTaxCents(-113000, 0.13), { amountCents: -100000, taxCents: -13000 });
  // 除不尽时也不能出现「不含税 + 税额 ≠ 合计」
  for (const total of [1, 7, 99, 12345, 999999, 333333]) {
    const { amountCents, taxCents } = splitTaxCents(total, 0.13);
    assert.equal(amountCents + taxCents, total, `合计 ${total} 分摊后不守恒`);
  }
  // 非法税率不炸：按 0 处理
  assert.deepEqual(splitTaxCents(10000, Number.NaN), { amountCents: 10000, taxCents: 0 });
});

test('税率文案：0.13 → 13%，0 → 0%，缺失 → —', () => {
  assert.equal(taxRateText('0.1300'), '13%');
  assert.equal(taxRateText(0.09), '9%');
  assert.equal(taxRateText(0), '0%');
  assert.equal(taxRateText(null), '—');
  assert.equal(taxRateText(''), '—');
  assert.equal(taxRateText('abc'), '—');
});

test('已开票合计排除 VOID，可排除自身', () => {
  const list = [
    row({ id: 'a', status: 'ISSUED', total_with_tax: '1130.0000' }),
    row({ id: 'b', status: 'DRAFT', total_with_tax: '100.0000' }),
    row({ id: 'c', status: 'VOID', total_with_tax: '500.0000' }),
    row({ id: 'd', status: 'POSTED', total_with_tax: '50.0000' }),
  ];
  assert.equal(invoicedTotalCents(list), 113000 + 10000 + 5000, '作废票不占额度');
  assert.equal(invoicedTotalCents(list, 'a'), 10000 + 5000);
  assert.equal(invoicedTotalCents([]), 0);
});

test('剩余额度 = 应收金额 − 已开票（作废后恢复；超额时为负，不许夹到 0）', () => {
  assert.equal(invoiceHeadroomCents('1130.0000', []), 113000);
  const partially = [row({ id: 'a', status: 'ISSUED', total_with_tax: '500.0000' })];
  assert.equal(invoiceHeadroomCents('1130.0000', partially), 63000);
  const voided = [row({ id: 'a', status: 'VOID', total_with_tax: '500.0000' })];
  assert.equal(invoiceHeadroomCents('1130.0000', voided), 113000, '作废后额度必须恢复');
  const over = [row({ id: 'a', status: 'ISSUED', total_with_tax: '1200.0000' })];
  assert.equal(invoiceHeadroomCents('1130.0000', over), -7000);
  // 应收金额从 Decimal 出 JSON 是字符串，不能靠 + 拼
  assert.equal(invoiceHeadroomCents('1130.0000', []), 113000);
});

// ==================== 开票表单 ====================

test('开票表单校验：应收必选 / 金额 > 0 / 不超额度 / 税率 0~1', () => {
  const ctx = { receivableAmount: '1130.0000', invoicedCents: 0, hasReceivable: true };
  assert.equal(invoiceFormError(form(), ctx), '');

  assert.match(invoiceFormError(form({ receivableId: '' }), ctx), /选择.*应收/);
  assert.match(invoiceFormError(form(), { ...ctx, hasReceivable: false }), /选择.*应收/);
  assert.match(invoiceFormError(form({ total: 0 }), ctx), /大于 0/);
  assert.match(invoiceFormError(form({ total: -1 }), ctx), /大于 0/);

  // 已开 500 ⇒ 只剩 630，开 630 通过、630.01 被拒
  const partial = { ...ctx, invoicedCents: 50000 };
  assert.equal(invoiceFormError(form({ total: 630 }), partial), '');
  assert.match(invoiceFormError(form({ total: 630.01 }), partial), /超过/);
  // 开满了：哪怕金额填 0 也要报「没额度」而不是「必须大于 0」
  assert.match(invoiceFormError(form({ total: 1 }), { ...ctx, invoicedCents: 113000 }), /额度/);
  assert.match(invoiceFormError(form({ total: 0 }), { ...ctx, invoicedCents: 113000 }), /额度/);

  assert.match(invoiceFormError(form({ taxRate: 1.5 }), ctx), /税率/);
  assert.match(invoiceFormError(form({ taxRate: -0.1 }), ctx), /税率/);
  // 0 税率合法（免税票）
  assert.equal(invoiceFormError(form({ taxRate: 0 }), ctx), '');

  assert.match(invoiceFormError(form({ invoiceNo: 'x'.repeat(65) }), ctx), /64/);
  assert.match(invoiceFormError(form({ invoiceCode: 'x'.repeat(33) }), ctx), /32/);
});

test('开票弹窗默认税率 0.13（后端 default_tax_rate）', () => {
  assert.equal(INVOICE_DEFAULT_TAX_RATE, 0.13);
  assert.equal(form().taxRate, 0.13);
});

// ==================== 作废原因 ====================

test('作废原因：按字符数 4~500，中文 2 字不算够长', () => {
  assert.equal(INVOICE_REASON_MIN, 4);
  assert.equal(INVOICE_REASON_MAX, 500);
  assert.match(invoiceReasonError(''), /至少 4 个/);
  assert.match(invoiceReasonError('作废'), /至少 4 个/, '中文 2 字是 2 个字符，不是 6 个');
  assert.match(invoiceReasonError('  开错  '), /至少 4 个/);
  assert.equal(invoiceReasonError('开错抬头'), '');
  assert.equal(invoiceReasonError('  开错抬头，重开  '), '');
  assert.match(invoiceReasonError('x'.repeat(501)), /最多 500/);
  assert.equal(invoiceReasonError('x'.repeat(500)), '');
});

// ==================== 筛选 ====================

test('筛选 → 查询参数：空串一律不发，模糊键 trim', () => {
  const empty = emptyInvoiceFilter();
  assert.equal(isInvoiceFiltered(empty), false);
  assert.deepEqual(buildInvoiceQuery(empty, 1), { page: 1, page_size: INVOICE_PAGE_SIZE });

  const full = {
    status: 'ISSUED' as const,
    invoiceType: 'OUT' as const,
    invoiceNo: '  IV-2026  ',
    dateFrom: '2026-11-01',
    dateTo: '2026-11-30',
  };
  assert.equal(isInvoiceFiltered(full), true);
  assert.deepEqual(buildInvoiceQuery(full, 2, 50), {
    page: 2,
    page_size: 50,
    status: 'ISSUED',
    invoice_type: 'OUT',
    invoice_no: 'IV-2026',
    date_from: '2026-11-01',
    date_to: '2026-11-30',
  });

  // 只有空白也算没筛（否则会发 `?invoice_no=`）
  assert.equal(isInvoiceFiltered({ ...empty, invoiceNo: '   ' }), false);
  const blank = buildInvoiceQuery({ ...empty, invoiceNo: '   ' }, 1);
  assert.equal('invoice_no' in blank, false);

  // 页码容错
  assert.equal(buildInvoiceQuery(empty, 0).page, 1);
  assert.equal(buildInvoiceQuery(empty, -3).page, 1);
  assert.equal(buildInvoiceQuery(empty, 2.9).page, 2);
});

// ==================== 下拉 / 列表 ====================

test('应收下拉文案含单号 / 客户 / 金额 / 到期日 / 已开票提示', () => {
  const label = receivableOptionLabel({
    ar_no: 'AR-20261106-000001',
    customer_name: '北京测试客户',
    amount: '1130.0000',
    due_date: '2026-12-06',
  });
  assert.match(label, /AR-20261106-000001/);
  assert.match(label, /北京测试客户/);
  assert.match(label, /1,130\.00/);
  assert.match(label, /到期 2026-12-06/);
  assert.equal(/已开票/.test(label), false);

  const invoiced = receivableOptionLabel({
    ar_no: 'AR-1',
    customer_name: '',
    amount: '10',
    due_date: '',
    invoice_no: 'IV-20261106-000001',
  });
  assert.match(invoiced, /客户已删除/);
  assert.match(invoiced, /已开票 IV-20261106-000001/);
});

test('列表列：9 列、单号固定左、操作固定右、宽度合计 ≤ 1288（1600 视口实测可视宽）', () => {
  const keys = INVOICE_COLUMNS.map((col) => col.colKey);
  assert.deepEqual(keys, [
    'invoice_no',
    'receivable_no',
    'customer_name',
    'invoice_date',
    'amount',
    'tax_amount',
    'total_with_tax',
    'status',
    'op',
  ]);
  assert.equal(INVOICE_COLUMNS[0]?.fixed, 'left');
  assert.equal(INVOICE_COLUMNS[INVOICE_COLUMNS.length - 1]?.fixed, 'right');
  const width = INVOICE_COLUMNS.reduce((sum, col) => sum + Number(col.width ?? 0), 0);
  assert.equal(width, 1130, `列宽合计变了（${width}）：先量 t-table__content 再改`);
  assert.ok(width <= 1288, '固定操作列会盖住状态列');
  // 金额列右对齐（财务口径：数字右对齐才对得齐）
  for (const key of ['amount', 'tax_amount', 'total_with_tax']) {
    const col = INVOICE_COLUMNS.find((item) => item.colKey === key);
    assert.equal(col?.align, 'right', `${key} 应右对齐`);
  }
});

test('4 个权限码与 V0296 的 sys_menu.auth_code 逐字一致', () => {
  assert.deepEqual(INVOICE_PERMS, {
    list: 'business:erp:invoice:list',
    detail: 'business:erp:invoice:detail',
    create: 'business:erp:invoice:create',
    status: 'business:erp:invoice:status',
  });
  // 状态下拉 4 项、票种下拉 2 项
  assert.deepEqual(
    INVOICE_STATUS_OPTIONS.map((item) => item.value),
    ['DRAFT', 'ISSUED', 'POSTED', 'VOID'],
  );
  assert.equal(INVOICE_PAGE_SIZE, 20);
});

// ==================== 后端源码契约（逐字） ====================

const HERE = dirname(fileURLToPath(import.meta.url));
const BACKEND = resolve(HERE, '../../../backend/src/business');

function readBackend(relative: string): string {
  return readFileSync(resolve(BACKEND, relative), 'utf8');
}

test('后端 invoice.rs 的状态域 / 原因长度 / 票种 / 额度口径与前端一致', () => {
  const src = readBackend('erp/invoice.rs');
  // 状态常量
  for (const constName of ['STATUS_DRAFT', 'STATUS_ISSUED', 'STATUS_POSTED', 'STATUS_VOID']) {
    assert.ok(src.includes(`pub const ${constName}`), `invoice.rs 缺少 ${constName}`);
  }
  assert.match(src, /pub const STATUSES: \[&str; 4\] = \[STATUS_DRAFT, STATUS_ISSUED, STATUS_POSTED, STATUS_VOID\]/);
  assert.match(src, /pub const REASON_MIN_CHARS: usize = 4;/);
  assert.match(src, /pub const REASON_MAX_CHARS: usize = 500;/);
  assert.match(src, /pub const TYPES: \[&str; 2\] = \[TYPE_OUT, TYPE_OUT_RED\]/);
  // 额度口径：SUM(total_with_tax) 且排除 VOID
  assert.ok(src.includes('COALESCE(SUM(total_with_tax), 0)'), '额度 sql 变了');
  assert.ok(src.includes('status <> ?'), '额度必须排除 VOID（status <> VOID）');
  // 状态中文（前端兜底表要与后端同词）
  for (const label of ['草稿', '已开票', '已归档', '已作废']) {
    assert.ok(src.includes(`"${label}"`), `后端状态中文缺「${label}」`);
  }
  // 4 个权限码
  for (const code of Object.values(INVOICE_PERMS)) {
    assert.ok(src.includes(code), `后端缺权限码 ${code}`);
  }
});

test('后端 state_machine.rs 的 INVOICE 边表 / 终态与前端逐条一致', () => {
  const src = readBackend('shared/state_machine.rs');
  const edges = /pub const INVOICE_EDGES: \[\(&str, &str\); 4\] = \[([\s\S]*?)\];/.exec(src);
  assert.ok(edges, '找不到 INVOICE_EDGES（后端改名了？）');
  const edgeTuples = [...(edges?.[1] ?? '').matchAll(/\("([A-Z_]+)", "([A-Z_]+)"\)/g)].map(
    (match) => [match[1], match[2]],
  );
  assert.deepEqual(edgeTuples, INVOICE_EDGES.map(([from, to]) => [from, to]));

  const reasonEdges = /pub const INVOICE_REASON_EDGES: \[\(&str, &str\); 2\] = \[([\s\S]*?)\];/.exec(src);
  assert.ok(reasonEdges, '找不到 INVOICE_REASON_EDGES');
  const reasonTuples = [...(reasonEdges?.[1] ?? '').matchAll(/\("([A-Z_]+)", "([A-Z_]+)"\)/g)].map(
    (match) => [match[1], match[2]],
  );
  assert.deepEqual(reasonTuples, INVOICE_REASON_EDGES.map(([from, to]) => [from, to]));

  const locked = /pub const INVOICE_LOCKED: \[&str; 3\] = \[([^\]]*)\]/.exec(src);
  assert.ok(locked, '找不到 INVOICE_LOCKED');
  assert.deepEqual(
    [...(locked?.[1] ?? '').matchAll(/"([A-Z_]+)"/g)].map((match) => match[1]),
    INVOICE_LOCKED,
  );
});
