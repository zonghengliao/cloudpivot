/**
 * `customer-query.ts` 单测（SO-03 客户档案页的纯逻辑）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * 为什么测 `.ts` 不测 `.vue`：用例跑在没有 DOM 的 node 里。「后端字段有没有被抄错」
 * 「状态是字符串域还是 1/0」「部分更新的请求体是不是只带改过的字段」「清空发的是
 * `''` 还是 `null`」全在 `customer-query.ts`；`.vue` 只负责渲染，这些一旦漂移
 * `vue-tsc` 抓不到（编译期两个都是 `string`）。
 *
 * 断言口径全部照后端抄（`backend/src/business/erp/customer_handler.rs` +
 * `customer_model.rs`），并用真接口 `GET /api/business/erp/customer/page` 核对过。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { Customer } from '@/api/business/erp';
import {
  CUSTOMER_COLUMNS,
  CUSTOMER_CREDIT_LIMIT_MAX,
  CUSTOMER_PAGE_SIZE,
  CUSTOMER_STATUS_OPTIONS,
  buildCustomerCreatePayload,
  buildCustomerQuery,
  buildCustomerStatusPayload,
  buildCustomerUpdatePayload,
  contactLabel,
  creditLimitText,
  customerFormFromRow,
  customerStatusLabel,
  customerStatusTheme,
  customerToggleLabel,
  emptyCustomerFilter,
  emptyCustomerForm,
  formatCreditLimit,
  hasCustomerFieldError,
  isCustomerFiltered,
  isCustomerListEmpty,
  isCustomerUpdateEmpty,
  nextCustomerStatus,
  textOr,
  validateCustomerForm,
} from '@/views/business/erp/customer/customer-query';

/** 一行真数据（形态照抄 `GET /api/business/erp/customer/page` 返回的 list[0]） */
function row(overrides: Partial<Customer> = {}): Customer {
  return {
    id: '019a3f60-0000-7000-8000-000000000001',
    code: 'CUS-0001',
    name: '北京示例科技有限公司',
    tax_no: '91110108MA01ABC123',
    contact_name: '张伟',
    contact_phone: '13800000001',
    address: '北京市海淀区中关村大街 1 号',
    credit_limit: '200000.5000',
    status: 'ACTIVE',
    remark: '种子客户',
    created_at: '2026-11-06T00:00:00Z',
    updated_at: '2026-11-06T00:00:00Z',
    ...overrides,
  };
}

test('CUSTOMER_COLUMNS 的 colKey 与后端 CustomerRes 逐字一致（不含 supplier_ 前缀）', () => {
  const keys = CUSTOMER_COLUMNS.map((col) => col.colKey);
  assert.deepEqual(keys, ['code', 'name', 'contact_name', 'credit_limit', 'status', 'op']);
  for (const key of keys) {
    // 供应商页的坑：照抄 supplier_code / supplier_name 会让整列空白且编译期不报错
    assert.ok(!String(key).includes('supplier_'), `列 ${String(key)} 抄成了供应商前缀`);
  }
});

test('CUSTOMER_STATUS_OPTIONS 是字符串域 ACTIVE/FROZEN，不是 1/0', () => {
  assert.deepEqual(
    CUSTOMER_STATUS_OPTIONS.map((item) => item.value),
    ['ACTIVE', 'FROZEN'],
  );
});

test('buildCustomerQuery：空筛选只发 page/page_size（空串发出去会查出 0 行）', () => {
  assert.deepEqual(buildCustomerQuery(emptyCustomerFilter(), 1), {
    page: 1,
    page_size: CUSTOMER_PAGE_SIZE,
  });
  assert.equal(CUSTOMER_PAGE_SIZE, 20);
});

test('buildCustomerQuery：code / name 各自 trim 后独立下发，status 精确透传', () => {
  assert.deepEqual(
    buildCustomerQuery({ code: '  CUS-00  ', name: ' 科技 ', status: 'FROZEN' }, 3, 50),
    { code: 'CUS-00', name: '科技', status: 'FROZEN', page: 3, page_size: 50 },
  );
});

test('isCustomerFiltered：任一条件有值即筛选态', () => {
  assert.equal(isCustomerFiltered(emptyCustomerFilter()), false);
  assert.equal(isCustomerFiltered({ code: '  ', name: '', status: '' }), false);
  assert.equal(isCustomerFiltered({ code: '', name: '科技', status: '' }), true);
  assert.equal(isCustomerFiltered({ code: '', name: '', status: 'ACTIVE' }), true);
});

test('状态文案 / 主题 / 切换目标', () => {
  assert.equal(customerStatusLabel('ACTIVE'), '正常');
  assert.equal(customerStatusLabel('FROZEN'), '冻结');
  assert.equal(customerStatusLabel('LEGACY'), '未知(LEGACY)');
  assert.equal(customerStatusLabel(null), '—');

  assert.equal(customerStatusTheme('ACTIVE'), 'success');
  assert.equal(customerStatusTheme('FROZEN'), 'warning');
  assert.equal(customerStatusTheme('LEGACY'), 'default');

  assert.equal(nextCustomerStatus('ACTIVE'), 'FROZEN');
  assert.equal(nextCustomerStatus('FROZEN'), 'ACTIVE');
  assert.equal(nextCustomerStatus(null), 'FROZEN');
  assert.equal(customerToggleLabel('ACTIVE'), '冻结');
  assert.equal(customerToggleLabel('FROZEN'), '解冻');
});

test('formatCreditLimit：字符串裁尾零，0 说人话「不控额度」', () => {
  assert.equal(formatCreditLimit('200000.5000'), '200000.5');
  assert.equal(formatCreditLimit('1000.0000'), '1000');
  assert.equal(formatCreditLimit('0.0000'), '不控额度');
  assert.equal(formatCreditLimit('0'), '不控额度');
  assert.equal(formatCreditLimit(null), '—');
  assert.equal(formatCreditLimit(''), '—');
});

test('creditLimitText：数字 → 字符串定点（不走 toFixed，避免二进制误差）', () => {
  assert.equal(creditLimitText(0), '0');
  assert.equal(creditLimitText(0.3), '0.3');
  assert.equal(creditLimitText(200000.5), '200000.5');
  // 负数 / NaN 一律折成 0（前端已有字段错误拦住，这里只是不让 0 以外的脏值漏出去）
  assert.equal(creditLimitText(-1), '0');
  assert.equal(creditLimitText(Number.NaN), '0');
});

test('contactLabel / textOr：空值统一显示 —', () => {
  assert.equal(contactLabel({ contact_name: '张伟', contact_phone: '138' }), '张伟 · 138');
  assert.equal(contactLabel({ contact_name: '张伟', contact_phone: null }), '张伟');
  assert.equal(contactLabel({ contact_name: null, contact_phone: '138' }), '— · 138');
  assert.equal(contactLabel({ contact_name: null, contact_phone: null }), '—');
  assert.equal(textOr('  '), '—');
  assert.equal(textOr('x'), 'x');
});

test('isCustomerListEmpty', () => {
  assert.equal(isCustomerListEmpty([]), true);
  assert.equal(isCustomerListEmpty(null), true);
  assert.equal(isCustomerListEmpty([{ id: 'a' }]), false);
});

test('validateCustomerForm：新建必填编码 + 名称，超长与负额度都要拦', () => {
  const errors = validateCustomerForm(emptyCustomerForm(), false);
  assert.equal(errors.code, '客户编码必填');
  assert.equal(errors.name, '客户名称必填');
  assert.equal(hasCustomerFieldError(errors), true);

  const longName = validateCustomerForm({ ...emptyCustomerForm(), code: 'C1', name: 'n'.repeat(201) }, false);
  assert.equal(longName.name, '客户名称最多 200 个字符');

  const longCode = validateCustomerForm({ ...emptyCustomerForm(), code: 'c'.repeat(41), name: 'n' }, false);
  assert.equal(longCode.code, '客户编码最多 40 个字符');

  const negative = validateCustomerForm(
    { ...emptyCustomerForm(), code: 'C1', name: 'n', credit_limit: -0.01 },
    false,
  );
  assert.equal(negative.credit_limit, '信用额度不能为负（0 = 不控额度）');

  const tooBig = validateCustomerForm(
    { ...emptyCustomerForm(), code: 'C1', name: 'n', credit_limit: CUSTOMER_CREDIT_LIMIT_MAX + 1 },
    false,
  );
  assert.equal(tooBig.credit_limit, '信用额度超出可存范围（整数位最多 14 位）');
});

test('validateCustomerForm：编辑态不再校验编码（只读，改不了）；0 额度合法', () => {
  const errors = validateCustomerForm(
    { ...emptyCustomerForm(), code: '', name: '北京示例科技', credit_limit: 0 },
    true,
  );
  assert.deepEqual(errors, {});

  const longText = validateCustomerForm(
    { ...emptyCustomerForm(), code: 'C1', name: 'n', contact_phone: '1'.repeat(31) },
    false,
  );
  assert.equal(longText.contact_phone, '联系电话最多 30 个字符');
});

test('buildCustomerCreatePayload：trim + 空串折 null + 额度转字符串定点 + 默认 ACTIVE', () => {
  const payload = buildCustomerCreatePayload({
    ...emptyCustomerForm(),
    code: '  CUS-9001  ',
    name: '  新客户  ',
    tax_no: '   ',
    contact_name: ' 李雷 ',
    contact_phone: '',
    address: '   ',
    credit_limit: 15000.25,
    remark: '',
  });
  assert.deepEqual(payload, {
    code: 'CUS-9001',
    name: '新客户',
    tax_no: null,
    contact_name: '李雷',
    contact_phone: null,
    address: null,
    credit_limit: '15000.25',
    status: 'ACTIVE',
    remark: null,
  });
});

test('buildCustomerUpdatePayload：没改就发空对象（省一次 PUT 和一条假成功）', () => {
  const original = row();
  const payload = buildCustomerUpdatePayload(customerFormFromRow(original), original);
  assert.deepEqual(payload, {});
  assert.equal(isCustomerUpdateEmpty(payload), true);
});

test('buildCustomerUpdatePayload：只发改过的字段，其余缺席', () => {
  const original = row();
  const form = customerFormFromRow(original);
  form.contact_name = '李雷';
  const payload = buildCustomerUpdatePayload(form, original);
  assert.deepEqual(payload, { contact_name: '李雷' });
});

test('buildCustomerUpdatePayload：文本从有到无发空串（后端约定 "" = 清空）', () => {
  const original = row();
  const form = customerFormFromRow(original);
  form.address = '';
  form.remark = '   ';
  const payload = buildCustomerUpdatePayload(form, original);
  assert.deepEqual(payload, { address: '', remark: '' });
});

test('buildCustomerUpdatePayload：额度与状态分开比较（0.0000 等于 0 不算改）', () => {
  const original = row({ credit_limit: '0.0000' });
  const form = customerFormFromRow(original);
  assert.deepEqual(buildCustomerUpdatePayload(form, original), {});

  form.credit_limit = 5000;
  assert.deepEqual(buildCustomerUpdatePayload(form, original), { credit_limit: '5000' });

  form.status = 'FROZEN';
  assert.deepEqual(buildCustomerUpdatePayload(form, original), {
    credit_limit: '5000',
    status: 'FROZEN',
  });
});

test('buildCustomerStatusPayload：列表上的冻结/解冻只带 status', () => {
  assert.deepEqual(buildCustomerStatusPayload('FROZEN'), { status: 'FROZEN' });
  assert.deepEqual(buildCustomerStatusPayload('ACTIVE'), { status: 'ACTIVE' });
});

test('customerFormFromRow：FROZEN 原样回填，null 文本折成空串（t-input 不吃 null）', () => {
  const form = customerFormFromRow(
    row({ status: 'FROZEN', tax_no: null, contact_name: null, contact_phone: null, address: null, remark: null, credit_limit: '0.0000' }),
  );
  assert.equal(form.status, 'FROZEN');
  assert.equal(form.code, 'CUS-0001');
  assert.equal(form.name, '北京示例科技有限公司');
  assert.equal(form.tax_no, '');
  assert.equal(form.contact_name, '');
  assert.equal(form.contact_phone, '');
  assert.equal(form.address, '');
  assert.equal(form.remark, '');
  assert.equal(form.credit_limit, 0);

  // 脏状态不能让界面进入「未知」态：非 FROZEN 一律按 ACTIVE
  assert.equal(
    customerFormFromRow(row({ status: 'LEGACY' as unknown as Customer['status'] })).status,
    'ACTIVE',
  );
});
