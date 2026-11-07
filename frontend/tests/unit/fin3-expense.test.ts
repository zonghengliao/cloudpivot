/**
 * FIN-3 费用报销三页的纯逻辑单测（我的报销 / 待付款 / 费用类型）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零依赖、无 DOM）。
 *
 * 测 `.ts` 不测 `.vue`：这页最容易错的是**口径** ——
 * 含税怎么拆（一次舍入 vs 两次）、招待费为什么税额必须是 0、
 * 哪些状态能提交 / 撤回 / 作废 / 付款、作废原因为什么卡 10 字。
 * 这些 `vue-tsc` 抓不到，页面渲染也看不出来。
 *
 * 断言口径照后端抄：
 * - `expense_compute::split`（`base = round2(with_tax/(1+rate))`、`tax = with_tax − base`、
 *   不可抵扣恒 0）
 * - `expense_dto::validate_report / validate_pay / validate_type`
 * - 状态机（`DRAFT→SUBMITTED→APPROVED→PAID`、撤回、作废前置条件）
 *
 * 最后几条是本页对 n08（菜单路径可解析）的镜像：三条菜单的 path / component
 * 从**迁移 V0242** 里读出来，再逐条钉住路由与视图文件真实存在 ——
 * 防止哪天删了文件却以为菜单还能点（FIN-1 踩过的坑）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  budgetFlag,
  budgetUsageRows,
  canCancelReport,
  canDeleteReport,
  canEditReport,
  canPayReport,
  canSubmitReport,
  canWithdrawReport,
  cancelReasonError,
  centsText,
  emptyExpenseForm,
  emptyItemForm,
  emptyPayForm,
  emptyTypeForm,
  firstError,
  formFromDetail,
  invoiceTypeText,
  MIN_CANCEL_REASON_LEN,
  NON_DEDUCTIBLE_HINT,
  payMethodText,
  paymentStatusText,
  payStatusText,
  payStatusTheme,
  previewItem,
  previewTotals,
  rateText,
  splitPreview,
  statusText,
  statusTheme,
  sumSplits,
  toPayPayload,
  toReportPayload,
  toTypePayload,
  typeDeductible,
  TYPE_CODE_PATTERN,
  usageStatusText,
  validateExpenseForm,
  validatePayForm,
  validateTypeForm,
  type ExpenseForm,
  type ExpenseItemForm,
} from '@/views/business/erp/expense/expense-logic';
import type {
  ExpenseBudgetUsage,
  ExpenseReport,
  ExpenseType,
} from '@/api/business/erp/expense';

const HERE = dirname(fileURLToPath(import.meta.url));
/** `frontend/` 根目录（`tests/unit` 上两级） */
const FRONTEND_ROOT = resolve(HERE, '../..');

// ==================== 夹具 ====================

/** 一条费用类型（只填被测字段，其余给安全默认值） */
function makeType(overrides: Partial<ExpenseType> = {}): ExpenseType {
  return {
    id: 'type-1',
    type_code: 'TRAVEL',
    type_name: '差旅费',
    account_id: 'acc-5602',
    account_code: '5602',
    account_name: '管理费用',
    tax_deductible: true,
    budget_controlled: true,
    need_invoice_no: false,
    amount_limit: '0.0000',
    sort_order: 10,
    status: 1,
    remark: null,
    ...overrides,
  };
}

/** 招待费：不可抵扣 */
const ENTERTAIN = makeType({
  id: 'type-ent',
  type_code: 'ENTERTAIN',
  type_name: '业务招待费',
  tax_deductible: false,
  amount_limit: '0.0000',
});

/** 一行明细表单态 */
function makeItem(overrides: Partial<ExpenseItemForm> = {}): ExpenseItemForm {
  return { ...emptyItemForm(), ...overrides };
}

/** 报销单夹具 */
function makeReport(overrides: Partial<ExpenseReport> = {}): ExpenseReport {
  return {
    id: 'report-1',
    report_no: 'BX-20261105-000001',
    reporter_id: 'user-1',
    reporter_name: '张三',
    dept_id: 'dept-1',
    dept_name: '研发部',
    title: '差旅报销',
    expense_date: '2026-11-01',
    period_code: '2026-11',
    total_amount: '6226.4200',
    total_tax: '373.5800',
    total_with_tax: '6600.0000',
    item_count: 1,
    status: 'DRAFT',
    status_label: '草稿',
    pay_status: 'UNPAID',
    pay_status_label: '未付款',
    paid_amount: '0.0000',
    unpaid_amount: '6600.0000',
    last_pay_date: null,
    over_budget: false,
    over_amount: '0.0000',
    approval_instance_id: null,
    voucher_id: null,
    voucher_no: null,
    submitted_at: null,
    approved_at: null,
    cancel_reason: null,
    remark: null,
    created_at: '2026-11-05T02:00:00Z',
    ...overrides,
  };
}

// ==================== 价税拆分（口径 = expense_compute::split） ====================

test('splitPreview：6600 含税 / 6% → 不含税 6,226.42 + 税额 373.58（差额法，只舍入一次）', () => {
  const split = splitPreview('6600', '0.06', true);
  assert.equal(split.withTaxCents, 660_000);
  assert.equal(split.amountCents, 622_642);
  assert.equal(split.taxCents, 37_358);
  // 合计恒等：不含税 + 税额 = 含税（这是月末对账的底线）
  assert.equal(split.amountCents + split.taxCents, split.withTaxCents);
  assert.equal(centsText(split.amountCents), '6,226.42');
  assert.equal(centsText(split.taxCents), '373.58');
});

test('splitPreview：不可抵扣（招待费）→ 税额恒 0、不含税 = 含税', () => {
  const split = splitPreview('6600', '0.06', false);
  assert.equal(split.withTaxCents, 660_000);
  assert.equal(split.amountCents, 660_000);
  assert.equal(split.taxCents, 0);
});

test('splitPreview：税率 0 / 空值 / 非法值 → 税额 0（不抛异常）', () => {
  for (const rate of ['0', '', null, undefined, 'abc']) {
    const split = splitPreview('100.00', rate, true);
    assert.equal(split.taxCents, 0, `rate=${String(rate)} 时税额应为 0`);
    assert.equal(split.amountCents, 10_000);
  }
});

test('splitPreview：税额取差额而不是 round(含税×税率)，避免多出一分', () => {
  // 100.00 / 1.13 = 88.495575… → 不含税 88.50、税额 11.50（若独立舍入 100×0.13 = 13.00 就错）
  const split = splitPreview('100.00', '0.13', true);
  assert.equal(split.amountCents, 8_850);
  assert.equal(split.taxCents, 1_150);
  assert.equal(split.amountCents + split.taxCents, 10_000);
});

test('sumSplits：逐行先拆再累加（不是先累加再拆）', () => {
  const splits = [
    splitPreview('6600', '0.06', true),
    splitPreview('500', '0.06', false),
  ];
  const totals = sumSplits(splits);
  assert.equal(totals.itemCount, 2);
  assert.equal(totals.withTaxCents, 710_000);
  assert.equal(totals.taxCents, 37_358);
  assert.equal(totals.amountCents, 710_000 - 37_358);
});

// ==================== 类型可抵扣 / 行预览 ====================

test('typeDeductible：找不到类型时按可抵扣处理（最终以后端裁决为准）', () => {
  assert.equal(typeDeductible(ENTERTAIN.id, [makeType(), ENTERTAIN]), false);
  assert.equal(typeDeductible('type-1', [makeType(), ENTERTAIN]), true);
  assert.equal(typeDeductible('missing', [makeType(), ENTERTAIN]), true);
});

test('previewItem：选业务招待费 → 行税额 0，且提示文案是 §4.10 原文', () => {
  const item = makeItem({ expense_type_id: ENTERTAIN.id, with_tax_amount: '6600', tax_rate: '0.06' });
  const preview = previewItem(item, [makeType(), ENTERTAIN]);
  assert.equal(preview.taxCents, 0);
  assert.equal(preview.amountCents, 660_000);
  assert.equal(NON_DEDUCTIBLE_HINT, '招待费进项税不得抵扣');
});

test('previewTotals：两种类型混排仍按各自可抵扣性拆', () => {
  const form = {
    items: [
      makeItem({ expense_type_id: 'type-1', with_tax_amount: '6600', tax_rate: '0.06' }),
      makeItem({ expense_type_id: ENTERTAIN.id, with_tax_amount: '1000', tax_rate: '0.06' }),
    ],
  };
  const totals = previewTotals(form, [makeType(), ENTERTAIN]);
  assert.equal(totals.withTaxCents, 760_000);
  assert.equal(totals.taxCents, 37_358);
  assert.equal(totals.amountCents, 760_000 - 37_358);
});

test('rateText：0.0600 → 6%，0.13 → 13%，脏值原样返回', () => {
  assert.equal(rateText('0.0600'), '6%');
  assert.equal(rateText('0.13'), '13%');
  assert.equal(rateText(0), '0%');
  assert.equal(rateText('dirty'), 'dirty');
});

// ==================== 状态与动作可用性 ====================

test('状态配色：DRAFT 默认 / SUBMITTED 警告 / APPROVED 主色 / PAID 成功 / CANCELLED 危险', () => {
  assert.equal(statusTheme('DRAFT'), 'default');
  assert.equal(statusTheme('SUBMITTED'), 'warning');
  assert.equal(statusTheme('APPROVED'), 'primary');
  assert.equal(statusTheme('PAID'), 'success');
  assert.equal(statusTheme('CANCELLED'), 'danger');
  assert.equal(statusTheme('WHATEVER'), 'default');
});

test('statusText / payStatusText：优先后端下发的 label（前端不维护第二份译名）', () => {
  assert.equal(statusText({ status: 'APPROVED', status_label: '后端文案' }), '后端文案');
  assert.equal(statusText({ status: 'APPROVED', status_label: '' }), '已审批（待付款）');
  assert.equal(payStatusText({ pay_status: 'PARTIAL', pay_status_label: '' }), '部分付款');
  assert.equal(payStatusTheme('PARTIAL'), 'warning');
});

test('动作可用性：只有草稿能改 / 删 / 提交，只有审批中能撤回', () => {
  const draft = { status: 'DRAFT' };
  const submitted = { status: 'SUBMITTED' };
  const approved = { status: 'APPROVED' };
  const paid = { status: 'PAID' };
  const cancelled = { status: 'CANCELLED' };

  assert.equal(canEditReport(draft), true);
  assert.equal(canDeleteReport(draft), true);
  assert.equal(canSubmitReport(draft), true);
  for (const other of [submitted, approved, paid, cancelled]) {
    assert.equal(canEditReport(other), false);
    assert.equal(canDeleteReport(other), false);
    assert.equal(canSubmitReport(other), false);
  }

  assert.equal(canWithdrawReport(submitted), true);
  assert.equal(canWithdrawReport(draft), false);
  assert.equal(canWithdrawReport(approved), false);
});

test('作废可用性：PAID / 已作废不许作废；已付过一分钱也不许（要先红冲付款）', () => {
  const base = { status: 'APPROVED', paid_amount: '0.0000' };
  assert.equal(canCancelReport(base), true);
  assert.equal(canCancelReport({ status: 'DRAFT', paid_amount: '0' }), true);
  assert.equal(canCancelReport({ ...base, paid_amount: '0.0100' }), false);
  assert.equal(canCancelReport({ status: 'PAID', paid_amount: '6600.0000' }), false);
  assert.equal(canCancelReport({ status: 'CANCELLED', paid_amount: '0' }), false);
});

test('付款可用性：只有 APPROVED 且还有未付余额可付（部分付款后仍可付）', () => {
  assert.equal(canPayReport({ status: 'APPROVED', unpaid_amount: '6600.0000' }), true);
  assert.equal(canPayReport({ status: 'APPROVED', unpaid_amount: '100.0000' }), true);
  assert.equal(canPayReport({ status: 'APPROVED', unpaid_amount: '0.0000' }), false);
  assert.equal(canPayReport({ status: 'SUBMITTED', unpaid_amount: '6600.0000' }), false);
  assert.equal(canPayReport({ status: 'PAID', unpaid_amount: '0.0000' }), false);
});

// ==================== 作废原因（后端 <10 字 400） ====================

test('cancelReasonError：空 / 少于 10 字报错，10 字通过', () => {
  assert.match(cancelReasonError('') ?? '', /必填/);
  assert.match(cancelReasonError('太短') ?? '', /至少 10 个字/);
  // 9 个字
  assert.notEqual(cancelReasonError('一二三四五六七八九'), null);
  // 10 个字
  assert.equal(cancelReasonError('作废原因写清楚十个字'), null);
  // 前后空白不计入
  assert.notEqual(cancelReasonError('   一二三四五六七八九   '), null);
  assert.equal(MIN_CANCEL_REASON_LEN, 10);
});

// ==================== 表单校验（镜像 validate_report） ====================

test('validateExpenseForm：事由必填、明细至少一行', () => {
  const form: ExpenseForm = { ...emptyExpenseForm(), title: '  ', items: [] };
  const errors = validateExpenseForm(form, [makeType()]);
  assert.ok(errors.title);
  assert.ok(errors.items);
  assert.match(firstError(errors), /事由/);
});

test('validateExpenseForm：行级错误定位到 items.<index>.<field>', () => {
  const form: ExpenseForm = {
    ...emptyExpenseForm(),
    title: '差旅',
    items: [
      makeItem({ expense_type_id: '', with_tax_amount: '', summary: '' }),
      makeItem({ expense_type_id: 'type-1', with_tax_amount: '0', summary: '高铁票' }),
    ],
  };
  const errors = validateExpenseForm(form, [makeType()]);
  assert.match(errors['items.0.expense_type_id'] ?? '', /第 1 行请选择费用类型/);
  assert.match(errors['items.0.with_tax_amount'] ?? '', /第 1 行请填含税金额/);
  assert.match(errors['items.1.with_tax_amount'] ?? '', /第 2 行含税金额必须大于 0/);
  assert.match(errors['items.0.summary'] ?? '', /第 1 行摘要必填/);
  assert.equal(errors['items.1.summary'], undefined);
});

test('validateExpenseForm：need_invoice_no 与 amount_limit 是行级业务校验', () => {
  const strict = makeType({ id: 'type-strict', need_invoice_no: true, amount_limit: '1000.0000' });
  const form: ExpenseForm = {
    ...emptyExpenseForm(),
    title: '差旅',
    items: [makeItem({ expense_type_id: strict.id, with_tax_amount: '1000.01', summary: '机票', invoice_no: '' })],
  };
  const errors = validateExpenseForm(form, [strict]);
  assert.match(errors['items.0.invoice_no'] ?? '', /必须填发票号/);
  assert.match(errors['items.0.with_tax_amount'] ?? '', /超过「差旅费」单笔上限/);

  // 上限内 + 有发票号 → 通过
  const ok = validateExpenseForm(
    { ...form, items: [makeItem({ expense_type_id: strict.id, with_tax_amount: '1000.00', summary: '机票', invoice_no: 'INV-1' })] },
    [strict],
  );
  assert.deepEqual(ok, {});
});

test('validateExpenseForm：税率超出 0~1 报错（6% 要填 0.06 不是 6）', () => {
  const form: ExpenseForm = {
    ...emptyExpenseForm(),
    title: '差旅',
    items: [makeItem({ expense_type_id: 'type-1', with_tax_amount: '100', summary: '票', tax_rate: '6' })],
  };
  const errors = validateExpenseForm(form, [makeType()]);
  assert.match(errors['items.0.tax_rate'] ?? '', /0~1/);
});

test('toReportPayload：只提交含税，空串字段不发（空 reporter_id 会被后端当 UUID 解析 400）', () => {
  const form: ExpenseForm = {
    title: '  差旅  ',
    expense_date: '2026-11-01',
    reporter_id: '   ',
    dept_id: '',
    remark: '  ',
    items: [
      makeItem({
        expense_type_id: 'type-1',
        with_tax_amount: '6600',
        tax_rate: '0.06',
        summary: ' 机票 ',
        invoice_no: '',
        invoice_date: '',
        invoice_type: '',
      }),
    ],
  };
  const payload = toReportPayload(form);
  assert.equal(payload.title, '差旅');
  assert.equal(payload.reporter_id, undefined);
  assert.equal(payload.dept_id, undefined);
  assert.equal(payload.remark, undefined);
  assert.equal(payload.items.length, 1);
  const item = payload.items[0]!;
  assert.equal(item.with_tax_amount, '6600');
  assert.equal(item.tax_rate, '0.06');
  assert.equal(item.summary, '机票');
  assert.equal(item.invoice_no, undefined);
  assert.equal(item.invoice_type, undefined);
  // 只录含税：payload 里不该出现 amount / tax_amount
  assert.equal('amount' in item, false);
  assert.equal('tax_amount' in item, false);
});

test('formFromDetail：编辑草稿时把后端拆好的含税金额填回去', () => {
  const detail = {
    report: makeReport(),
    items: [
      {
        id: 'item-1',
        line_no: 1,
        expense_date: '2026-11-01',
        expense_type_id: 'type-1',
        type_code: 'TRAVEL',
        type_name: '差旅费',
        account_id: 'acc-5602',
        account_code: '5602',
        account_name: '管理费用',
        amount: '6226.4200',
        tax_rate: '0.0600',
        tax_amount: '373.5800',
        with_tax_amount: '6600.0000',
        invoice_no: null,
        invoice_date: null,
        invoice_type: null,
        summary: '机票',
        remark: null,
      },
    ],
  };
  const form = formFromDetail(detail);
  assert.equal(form.title, detail.report.title);
  assert.equal(form.items.length, 1);
  assert.equal(form.items[0]!.with_tax_amount, '6600.0000');
  assert.equal(form.items[0]!.tax_rate, '0.06');
  assert.equal(form.items[0]!.invoice_no, '');
});

// ==================== 付款 ====================

test('emptyPayForm：金额预填**未付余额**（不是报销总额）', () => {
  const form = emptyPayForm({ unpaid_amount: '1234.5600' });
  assert.equal(form.amount, '1234.56');
  assert.equal(form.pay_method, 'BANK_TRANSFER');
  assert.equal(form.pay_account_code, '1002');
  assert.match(form.pay_date, /^\d{4}-\d{2}-\d{2}$/);
});

test('validatePayForm：金额 > 0 且 ≤ 未付，付款日期必填', () => {
  const form = emptyPayForm({ unpaid_amount: '1000' });
  assert.equal(validatePayForm(form, '1000'), null);
  assert.match(validatePayForm({ ...form, amount: '' }, '1000') ?? '', /必填/);
  assert.match(validatePayForm({ ...form, amount: '0' }, '1000') ?? '', /大于 0/);
  assert.match(validatePayForm({ ...form, amount: '1000.01' }, '1000') ?? '', /不能超过未付金额 1,000.00/);
  assert.match(validatePayForm({ ...form, pay_date: '' }, '1000') ?? '', /付款日期必填/);
});

test('toPayPayload：空账户 / 空备注不发', () => {
  const payload = toPayPayload({
    amount: ' 100 ',
    pay_date: '2026-11-05',
    pay_method: 'CASH',
    pay_account_code: '  ',
    remark: '',
  });
  assert.equal(payload.amount, '100');
  assert.equal(payload.pay_method, 'CASH');
  assert.equal(payload.pay_account_code, undefined);
  assert.equal(payload.remark, undefined);
});

// ==================== 费用类型 ====================

test('validateTypeForm：编码大写字母开头、科目必填、上限非负', () => {
  assert.match(validateTypeForm(emptyTypeForm()) ?? '', /编码必填/);
  assert.equal(TYPE_CODE_PATTERN.test('TRAVEL'), true);
  assert.equal(TYPE_CODE_PATTERN.test('TRAVEL_2026'), true);
  assert.equal(TYPE_CODE_PATTERN.test('travel'), false);
  assert.equal(TYPE_CODE_PATTERN.test('1TRAVEL'), false);
  assert.equal(TYPE_CODE_PATTERN.test('TRA-VEL'), false);

  const base = { ...emptyTypeForm(), type_code: 'travel', type_name: '差旅费', account_id: 'acc-1' };
  // 小写会被大写化后再校验（后端同样先 upper）
  assert.equal(validateTypeForm(base), null);
  assert.match(validateTypeForm({ ...base, account_id: '' }) ?? '', /借方费用科目/);
  assert.match(validateTypeForm({ ...base, amount_limit: '-1' }) ?? '', /不能为负/);
});

test('toTypePayload：编码大写化、0 上限原样提交、备注空串不发', () => {
  const payload = toTypePayload({
    ...emptyTypeForm(),
    type_code: ' travel ',
    type_name: ' 差旅费 ',
    account_id: 'acc-1',
    remark: '',
  });
  assert.equal(payload.type_code, 'TRAVEL');
  assert.equal(payload.type_name, '差旅费');
  assert.equal(payload.amount_limit, '0');
  assert.equal(payload.remark, undefined);
});

// ==================== 详情展示 ====================

test('budgetUsageRows：四要素齐全（部门 / 科目 / 占多少 / 占哪个月）', () => {
  const usage: ExpenseBudgetUsage = {
    id: 'usage-1',
    dept_id: 'dept-1',
    account_id: 'acc-5602',
    account_code: '5602',
    fiscal_year: 2026,
    period_code: '2026-11',
    amount: '6600.0000',
    status: 'OCCUPIED',
    over_budget: true,
    over_amount: '600.0000',
    release_reason: null,
  };
  const rows = budgetUsageRows(
    [usage],
    (id) => (id === 'dept-1' ? '研发部' : ''),
    (id) => (id === 'acc-5602' ? '管理费用' : ''),
  );
  assert.equal(rows.length, 1);
  assert.equal(rows[0]!.dept_name, '研发部');
  assert.equal(rows[0]!.account_text, '5602 管理费用');
  assert.equal(rows[0]!.amount, '6600.0000');
  assert.equal(rows[0]!.period_text, '2026-11');
  assert.equal(rows[0]!.status_text, '占用中');
  assert.equal(rows[0]!.over_budget, true);
});

test('budgetFlag：真超支标红、未编预算标黄、正常为空（FIX-5 列表页区分）', () => {
  // 真超支：有预算、花过了 → 红「超预算」
  assert.deepEqual(budgetFlag({ over_budget: true, missing_budget: false }), {
    text: '超预算',
    theme: 'danger',
  });
  // 没编预算：不是超支 → 黄「未编预算」（修复前这里也是红「超预算」）
  assert.deepEqual(budgetFlag({ over_budget: false, missing_budget: true }), {
    text: '未编预算',
    theme: 'warning',
  });
  // 两者都 false → 正常，页面显示 —
  assert.equal(budgetFlag({ over_budget: false, missing_budget: false }), null);
  // 两者同时 true（多槽混合：一个真超支、一个没编预算）→ 红「超预算」优先
  assert.deepEqual(budgetFlag({ over_budget: true, missing_budget: true }), {
    text: '超预算',
    theme: 'danger',
  });
});

test('预算占用 / 付款 / 发票 / 方式的中文映射（未知值原样暴露脏数据）', () => {
  // 三档都要有：OCCUPIED（提交时占用）/ CONSUMED（审批入账后转已消耗）/ RELEASED（撤回或作废释放）
  assert.equal(usageStatusText('OCCUPIED'), '占用中');
  assert.equal(usageStatusText('CONSUMED'), '已消耗');
  assert.equal(usageStatusText('RELEASED'), '已释放');
  assert.equal(usageStatusText('WEIRD'), 'WEIRD');
  assert.equal(paymentStatusText('APPROVED'), '已付款');
  assert.equal(paymentStatusText('PENDING'), '审批中');
  assert.equal(invoiceTypeText('VAT_SPECIAL'), '增值税专用发票');
  assert.equal(payMethodText('BANK_TRANSFER'), '银行转账');
  assert.equal(payMethodText(null), '—');
});

// ==================== 菜单 / 路由 / 接口契约（n08 镜像） ====================

test('三页菜单 path / component 与迁移 V0242 逐字一致', () => {
  const migration = readFileSync(
    resolve(FRONTEND_ROOT, '../backend/migrations/business/V0242__erp_expense_tables.up.sql'),
    'utf8',
  );

  const expected = [
    { path: '/business/erp/expense', component: 'business/erp/expense/index', file: 'src/views/business/erp/expense/index.vue' },
    { path: '/business/erp/expense/pay', component: 'business/erp/expense/pay/index', file: 'src/views/business/erp/expense/pay/index.vue' },
    { path: '/business/erp/expense-type', component: 'business/erp/expense-type/index', file: 'src/views/business/erp/expense-type/index.vue' },
  ];

  for (const item of expected) {
    assert.ok(migration.includes(`'${item.path}'`), `迁移里没有菜单 path：${item.path}`);
    assert.ok(migration.includes(`'${item.component}'`), `迁移里没有组件路径：${item.component}`);
    assert.ok(existsSync(resolve(FRONTEND_ROOT, item.file)), `视图文件不存在（菜单会 404）：${item.file}`);
  }
});

test('router 手工注册了三条路由（漏一条 = 菜单可见但点进去 404）', () => {
  const routerSource = readFileSync(resolve(FRONTEND_ROOT, 'src/router/index.ts'), 'utf8');
  for (const [path, view] of [
    ['business/erp/expense', '@/views/business/erp/expense/index.vue'],
    ['business/erp/expense/pay', '@/views/business/erp/expense/pay/index.vue'],
    ['business/erp/expense-type', '@/views/business/erp/expense-type/index.vue'],
  ]) {
    assert.ok(routerSource.includes(`path: '${path}'`), `缺少路由：${path}`);
    assert.ok(routerSource.includes(`component: () => import('${view}')`), `路由组件不对：${view}`);
  }
});

test('API 层路径全部写全 /api（漏了会命中 SPA fallback 返回 HTML）', () => {
  const apiSource = readFileSync(resolve(FRONTEND_ROOT, 'src/api/business/erp/expense.ts'), 'utf8');
  assert.ok(apiSource.includes("const BASE = '/api/business/erp'"));
  // 不允许出现裸的 '/business/erp/...' 注解路径
  assert.equal(/'\/business\/erp\//.test(apiSource), false, '存在没带 /api 前缀的路径');
  for (const path of [
    '/expense/page',
    '/expense/pay/page',
    '/expense-type/page',
    '/expense-type/list',
  ]) {
    assert.ok(apiSource.includes(path), `接口路径缺失：${path}`);
  }
});

test('权限码只取 V0242 种的 12 个（白名单外一个都不加）', () => {
  const whitelist = new Set([
    'business:erp:expense:list',
    'business:erp:expense:create',
    'business:erp:expense:update',
    'business:erp:expense:delete',
    'business:erp:expense:submit',
    'business:erp:expense:cancel',
    'business:erp:expense:detail',
    'business:erp:expense:pay',
    'business:erp:expense-type:list',
    'business:erp:expense-type:create',
    'business:erp:expense-type:update',
    'business:erp:expense-type:delete',
  ]);

  const files = [
    'src/views/business/erp/expense/index.vue',
    'src/views/business/erp/expense/pay/index.vue',
    'src/views/business/erp/expense/ExpenseDetailDrawer.vue',
    'src/views/business/erp/expense-type/index.vue',
  ];
  for (const file of files) {
    const source = readFileSync(resolve(FRONTEND_ROOT, file), 'utf8');
    for (const match of source.matchAll(/business:erp:expense[a-z-]*:[a-z]+/g)) {
      assert.ok(whitelist.has(match[0]), `${file} 用了白名单外的权限码：${match[0]}`);
    }
  }
});

test('「我的报销」默认只查自己 + 招待费税额置灰（源码契约）', () => {
  const source = readFileSync(resolve(FRONTEND_ROOT, 'src/views/business/erp/expense/index.vue'), 'utf8');
  assert.ok(source.includes('const mineOnly = ref(true)'), '默认口径必须是「只看我的」');
  assert.ok(source.includes('query.reporter_id = myId.value'), '必须把当前用户 id 传给 reporter_id');
  assert.ok(source.includes('tax-input--locked'), '不可抵扣的税额框要置灰');
  assert.ok(source.includes('NON_DEDUCTIBLE_HINT'), '必须显示「招待费进项税不得抵扣」提示');
  assert.ok(source.includes('expense-item-tax-'), '税额必须有独立输入框（置灰显示 0）');
});

test('详情抽屉四块必备信息都在（审批轨迹 / 凭证号 / 付款记录 / 预算占用）', () => {
  const source = readFileSync(resolve(FRONTEND_ROOT, 'src/views/business/erp/expense/ExpenseDetailDrawer.vue'), 'utf8');
  for (const marker of [
    'FlowProgress',
    'loadApprovalProgress',
    'voucher_no',
    'budget_usages',
    'expense-detail-budget-usage',
    'expense-detail-payments',
    'DocAttachmentPanel',
  ]) {
    assert.ok(source.includes(marker), `详情抽屉缺少：${marker}`);
  }
});
