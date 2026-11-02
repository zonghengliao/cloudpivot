/**
 * `unit-query.ts` / `supplier-query.ts` 单测（P4-A 计量单位 + 供应商主数据页）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * 为什么测 `.ts` 不测 `.vue`：用例跑在没有 DOM 的 node 里。「后端字段有没有被抄错」
 * 「停用是不是灰 tag」「清空折算基准发的是 `null` 还是空串」「筛选空串带不带」
 * 全在 `*-query.ts`；`.vue` 只负责渲染，这些一旦漂移 `vue-tsc` 抓不到。
 *
 * 断言口径全部照后端抄，并用真接口 curl 核对过：
 * - 计量单位 = `dto.rs::UnitRes`（`unit_code / unit_name / is_base / base_unit_id / rate /
 *   status / sort_order / remark / is_system / precision`）；
 *   末尾两列是 ERP-S1（V0187）补的：`is_system` 系统预置、`precision` 数量小数位数；
 * - 供应商 = `dto.rs::SupplierRes`（`supplier_code / supplier_name / ... / credit_days /
 *   credit_limit / payable_account_id / status`）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { Supplier, Unit } from '@/api/business/erp';
import {
  UNIT_COLUMNS,
  baseUnitOptions,
  buildUnitCreatePayload,
  buildUnitQuery,
  buildUnitUpdatePayload,
  emptyUnitFilter,
  emptyUnitForm,
  formatRate,
  isUnitListEmpty,
  rateLabel,
  unitBaseNameOf,
  unitBaseLabel,
  unitBaseTheme,
  UNIT_DOC_USAGE_HINT,
  unitDocUsageHint,
  unitFormFromRow,
  unitStatusLabel,
  unitStatusTheme,
  validateUnitForm,
  unitSourceLabel,
  unitSourceTheme,
  canRemoveUnit,
  precisionLabel,
} from '@/views/business/erp/unit/unit-query';
import {
  SUPPLIER_COLUMNS,
  buildSupplierCreatePayload,
  buildSupplierQuery,
  buildSupplierUpdatePayload,
  contactLabel,
  creditDaysLabel,
  emptySupplierFilter,
  formatAmount,
  isSupplierListEmpty,
  supplierFormFromRow,
  supplierStatusLabel,
  supplierStatusTheme,
  textOr,
  validateSupplierForm,
} from '@/views/business/erp/supplier/supplier-query';

/** 一条种子单位（值抄自真接口 `GET /erp/unit/page` 的 PCS 行） */
function seedUnit(overrides: Partial<Unit> = {}): Unit {
  return {
    id: 'd1370000-0000-4000-8000-000000000001',
    unit_code: 'PCS',
    unit_name: '个',
    is_base: true,
    base_unit_id: null,
    rate: '1.000000',
    status: 1,
    sort_order: 1,
    remark: null,
    is_system: true,
    precision: 0,
    created_at: '2026-10-30T08:52:10.282882Z',
    updated_at: '2026-10-30T08:52:10.282882Z',
    ...overrides,
  };
}

/** 一条种子供应商（值抄自真接口 `GET /erp/supplier/page` 的 SUP-0001 行） */
function seedSupplier(overrides: Partial<Supplier> = {}): Supplier {
  return {
    id: 'd1370005-0000-4000-8000-000000000001',
    supplier_code: 'SUP-0001',
    supplier_name: '示例供应商（种子）',
    short_name: '示例供应商',
    contact_person: null,
    contact_phone: null,
    email: null,
    tax_no: null,
    address: null,
    credit_days: 30,
    credit_limit: '0.0000',
    payable_account_id: null,
    invoice_title: null,
    invoice_bank_name: null,
    invoice_bank_account: null,
    status: 1,
    remark: null,
    created_at: '2026-10-30T08:52:10.282882Z',
    updated_at: '2026-10-30T08:52:10.282882Z',
    ...overrides,
  };
}

// ==================== ① 字段映射：后端字段 → 表格列 ====================

test('① 计量单位表格列 = 后端 UnitRes 的真字段（不自造列）', () => {
  const row = seedUnit();
  const colKeys = UNIT_COLUMNS.map((col) => String(col.colKey));
  assert.deepEqual(colKeys, [
    'unit_code',
    'unit_name',
    'is_system',
    'is_base',
    'rate',
    'precision',
    'sort_order',
    'status',
    'op',
  ]);

  // 除 `op` 外，每个 colKey 都必须能在真接口出参里找到同名键（拼错就是整列空白）
  for (const key of colKeys.filter((item) => item !== 'op')) {
    assert.ok(key in row, `列 ${key} 在 UnitRes 里不存在`);
  }

  // ERP-S1（V0187）后「小数位数」是真字段了：列里必须有，且不许退回当初的 decimal_places 假名
  assert.equal(colKeys.includes('precision'), true);
  assert.equal(colKeys.includes('decimal_places'), false);

  // 折算率列不能裸显示 "12.000000"：格式化后才是人看的
  assert.equal(formatRate('12.000000'), '12');
  assert.equal(formatRate('1.500000'), '1.5');
  assert.equal(formatRate('0.000001'), '0.000001');
  assert.equal(formatRate(null), '—');
});

test('① 供应商表格列 = 后端 SupplierRes 的真字段（不是 code / name）', () => {
  const row = seedSupplier();
  const colKeys = SUPPLIER_COLUMNS.map((col) => String(col.colKey));
  assert.deepEqual(colKeys, [
    'supplier_code',
    'supplier_name',
    'contact_person',
    'tax_no',
    'credit_days',
    'status',
    'op',
  ]);
  for (const key of colKeys.filter((item) => item !== 'op')) {
    assert.ok(key in row, `列 ${key} 在 SupplierRes 里不存在`);
  }
  // 猜成 code / name 的那套字段后端没有（列头点开是空白的根因）
  assert.equal(colKeys.includes('code'), false);
  assert.equal(colKeys.includes('name'), false);

  // 联系人 / 电话合并列：只有一边就只显示一边，都没有给 `—`
  assert.equal(contactLabel({ contact_person: '张三', contact_phone: '13800001111' }), '张三 · 13800001111');
  assert.equal(contactLabel({ contact_person: '张三', contact_phone: null }), '张三');
  assert.equal(contactLabel({ contact_person: null, contact_phone: '13800001111' }), '— · 13800001111');
  assert.equal(contactLabel({ contact_person: null, contact_phone: '  ' }), '—');
  assert.equal(textOr(null), '—');
  assert.equal(textOr('  示例 '), '示例');
});

// ==================== ② 状态映射：启用 / 停用 → tag 颜色 ====================

test('② 状态映射：1 启用绿 tag，0 停用灰 tag，脏值不染绿', () => {
  assert.equal(unitStatusLabel(1), '启用');
  assert.equal(unitStatusLabel(0), '停用');
  assert.equal(unitStatusLabel(9), '未知(9)');
  assert.equal(unitStatusTheme(1), 'success');
  assert.equal(unitStatusTheme(0), 'default');
  assert.equal(unitStatusTheme(9), 'default');

  assert.equal(supplierStatusLabel(1), '启用');
  assert.equal(supplierStatusLabel(0), '停用');
  assert.equal(supplierStatusTheme(1), 'success');
  assert.equal(supplierStatusTheme(0), 'default');
  // `status` 是 smallint，0 是**停用**不是「全部」—— 不能拿 0 当空值
  assert.equal(supplierStatusTheme(0), 'default');
});

// ==================== ③ 空态判断 ====================

test('③ 空态：rows.length === 0 才空，null / [] / 有行', () => {
  assert.equal(isUnitListEmpty([]), true);
  assert.equal(isUnitListEmpty(null), true);
  assert.equal(isUnitListEmpty(undefined), true);
  assert.equal(isUnitListEmpty([seedUnit()]), false);

  assert.equal(isSupplierListEmpty([]), true);
  assert.equal(isSupplierListEmpty(null), true);
  assert.equal(isSupplierListEmpty([seedSupplier()]), false);
});

// ==================== ④ 查询参数：空串不发、服务端分页字段名 ====================

test('④ 查询参数：空串不发，page/page_size 是服务端字段', () => {
  assert.deepEqual(buildUnitQuery(emptyUnitFilter(), 1), { page: 1, page_size: 20 });
  assert.deepEqual(buildUnitQuery({ keyword: '  KG  ', status: 1 }, 2), {
    page: 2,
    page_size: 20,
    keyword: 'KG',
    status: 1,
  });
  // status = 0（停用）必须发出去，不能被 `if (status)` 吞掉
  assert.deepEqual(buildUnitQuery({ keyword: '', status: 0 }, 1), { page: 1, page_size: 20, status: 0 });

  assert.deepEqual(buildSupplierQuery(emptySupplierFilter(), 1), { page: 1, page_size: 20 });
  assert.deepEqual(buildSupplierQuery({ keyword: ' SUP ', status: 0 }, 3), {
    page: 3,
    page_size: 20,
    keyword: 'SUP',
    status: 0,
  });
});

// ==================== ⑤ 计量单位：折算口径 + 请求体 ====================

test('⑤ 折算单位：1 箱 = 12 个，基准单位恒等 1，基准下拉剔除自己', () => {
  const box = seedUnit({ unit_code: 'BOX', unit_name: '箱', is_base: false, base_unit_id: 'pcs-id', rate: '12.000000' });
  const pcs = seedUnit({ id: 'pcs-id' });
  assert.equal(rateLabel(box, '个'), '1 = 12 个');
  assert.equal(rateLabel(pcs), '1（基准）');
  assert.equal(unitBaseNameOf(box, [pcs]), '个');
  assert.equal(unitBaseNameOf(pcs, [pcs]), ''); // 基础单位没有基准
  assert.equal(unitBaseNameOf(box, []), ''); // 查不到不显示 uuid

  // 下拉只列基础单位，且不能选自己（后端 ck_biz_unit_no_self 会 400）
  const options = baseUnitOptions([pcs, box], 'pcs-id');
  assert.deepEqual(options, []);
  assert.deepEqual(baseUnitOptions([pcs, box]), [{ label: '个（PCS）', value: 'pcs-id' }]);
});

test('⑤ 计量单位请求体：基础单位不发 base_unit_id，折算单位必须带 rate', () => {
  const base = buildUnitCreatePayload({ ...emptyUnitForm(), unit_code: ' KG ', unit_name: ' 千克 ' });
  assert.deepEqual(base, {
    unit_code: 'KG',
    unit_name: '千克',
    is_base: true,
    status: 1,
    sort_order: 0,
    remark: null,
    precision: 0,
    rate: 1,
  });
  assert.equal('base_unit_id' in base, false);

  const converted = buildUnitCreatePayload({
    ...emptyUnitForm(),
    unit_code: 'BOX',
    unit_name: '箱',
    is_base: false,
    base_unit_id: 'pcs-id',
    rate: 12,
  });
  assert.equal(converted.base_unit_id, 'pcs-id');
  assert.equal(converted.rate, 12);

  // 校验口径照 dto::CreateUnitReq::validate
  assert.deepEqual(validateUnitForm({ ...emptyUnitForm(), unit_code: '', unit_name: '' }, false), {
    unit_code: '单位编码必填',
    unit_name: '单位名称必填',
  });
  assert.deepEqual(
    validateUnitForm({ ...emptyUnitForm(), unit_code: 'BOX', unit_name: '箱', is_base: false, rate: 0 }, false),
    { base_unit_id: '折算单位必须选一个基础单位', rate: '折算率必须大于 0（1 个本单位等于几个基准单位）' },
  );
  // 编辑态不校验编码（后端 UpdateUnitReq 根本没有这个字段）
  assert.deepEqual(validateUnitForm({ ...emptyUnitForm(), unit_name: '箱' }, true), {});
});

test('⑤ 编辑请求体：只发改过的字段；清空折算基准必须发 null（不是空串）', () => {
  const original = seedUnit({ unit_code: 'BOX', unit_name: '箱', is_base: false, base_unit_id: 'pcs-id', rate: '12.000000' });

  // 什么都没改 → 空对象（后端缺席 = 不改，别整单覆盖别人的修改）
  assert.deepEqual(buildUnitUpdatePayload(unitFormFromRow(original), original), {});

  // 只改了名称
  const renamed = buildUnitUpdatePayload({ ...unitFormFromRow(original), unit_name: '小箱' }, original);
  assert.deepEqual(renamed, { unit_name: '小箱' });

  // 折算 → 基础：base_unit_id 发 null（double_option 语义），rate 不发（后端归 1）
  const asBase = buildUnitUpdatePayload({ ...unitFormFromRow(original), is_base: true, base_unit_id: '' }, original);
  assert.deepEqual(asBase, { base_unit_id: null });

  // 只改折算率
  const rate = buildUnitUpdatePayload({ ...unitFormFromRow(original), rate: 10 }, original);
  assert.deepEqual(rate, { rate: 10 });
});

// ==================== ⑥ 供应商：表单回填 / 请求体 ====================

test('⑥ 供应商表单回填与请求体：空串转 null，只发改过的字段', () => {
  const row = seedSupplier();
  const form = supplierFormFromRow(row);
  assert.equal(form.supplier_code, 'SUP-0001');
  assert.equal(form.short_name, '示例供应商');
  assert.equal(form.tax_no, ''); // null → 空串（输入框不显示 "null"）
  assert.equal(form.credit_days, 30);
  assert.equal(form.status, 1);

  // 新建：可选字段空串一律 null（别存成空串，列表看起来「有值但是空的」）
  const created = buildSupplierCreatePayload({
    ...form,
    supplier_code: ' SUP-0002 ',
    supplier_name: ' 新供应商 ',
    short_name: '  ',
    status: 0,
  });
  assert.equal(created.supplier_code, 'SUP-0002');
  assert.equal(created.supplier_name, '新供应商');
  assert.equal(created.short_name, null);
  assert.equal(created.status, 0);
  assert.equal(created.credit_days, 30);

  // 编辑什么都没改 → 空对象
  assert.deepEqual(buildSupplierUpdatePayload(supplierFormFromRow(row), row), {});

  // 清空联系人 / 税号 → null；改账期 → 数字
  const changed = buildSupplierUpdatePayload(
    { ...supplierFormFromRow(row), short_name: '', tax_no: '91310000XXX', credit_days: 60 },
    row,
  );
  assert.deepEqual(changed, { short_name: null, tax_no: '91310000XXX', credit_days: 60 });

  // 校验：账期 / 额度不能为负，邮箱要像邮箱
  assert.deepEqual(
    validateSupplierForm({ ...supplierFormFromRow(row), supplier_name: '', credit_days: -1, email: 'bad' }, true),
    {
      supplier_name: '供应商名称必填',
      email: '邮箱格式不对',
      credit_days: '账期天数不能为负',
    },
  );
});

test('⑥ 供应商展示：账期 0 = 现结，额度裁尾零', () => {
  assert.equal(creditDaysLabel(0), '现结（0 天）');
  assert.equal(creditDaysLabel(30), '30 天');
  assert.equal(creditDaysLabel(null), '—');
  assert.equal(formatAmount('0.0000'), '0');
  assert.equal(formatAmount('1500.5000'), '1500.5');
  assert.equal(formatAmount(null), '—');
});

// ==================== ⑦ ERP-S1：系统预置 / 小数位数 ====================

test('⑦ ERP-S1：系统预置单位打「系统」标签且不给删，客户自建可删', () => {
  // 来源标签：迁移种下的标准值 → 「系统」；客户自己建 → 「自建」
  assert.equal(unitSourceLabel(true), '系统');
  assert.equal(unitSourceLabel(false), '自建');
  assert.equal(unitSourceLabel(undefined), '自建', '缺字段（老缓存）按自建，不冒充预置');
  assert.equal(unitSourceTheme(true), 'warning');
  assert.equal(unitSourceTheme(false), 'default');

  // 删除入口：预置单位隐藏删除（后端也会 409），自建单位可删
  assert.equal(canRemoveUnit(seedUnit({ is_system: true })), false);
  assert.equal(canRemoveUnit(seedUnit({ is_system: false })), true);

  // 小数位数文案：0 位说明是整数，缺失显示破折号（不假装 0）
  assert.equal(precisionLabel(0), '0（整数）');
  assert.equal(precisionLabel(3), '3');
  assert.equal(precisionLabel(null), '—');
  assert.equal(precisionLabel(undefined), '—');
});

test('⑦ ERP-S1：小数位数进请求体（新建带上、编辑只发改过的）且校验 0–6', () => {
  const created = buildUnitCreatePayload({
    ...emptyUnitForm(),
    unit_code: 'G',
    unit_name: '克',
    precision: 3,
  });
  assert.equal(created.precision, 3, '新建要把小数位数发给后端');

  // 越界在前端就被拦（后端还有 ck_biz_unit_precision 兜底）
  assert.equal(validateUnitForm({ ...emptyUnitForm(), unit_code: 'X', unit_name: 'X', precision: 7 }, false).precision, '小数位数只能是 0–6 的整数');
  assert.equal(validateUnitForm({ ...emptyUnitForm(), unit_code: 'X', unit_name: 'X', precision: 1.5 }, false).precision, '小数位数只能是 0–6 的整数');
  assert.deepEqual(validateUnitForm({ ...emptyUnitForm(), unit_code: 'X', unit_name: 'X', precision: 3 }, false), {});

  const original = seedUnit({ is_system: false, precision: 0 });
  const unchanged = buildUnitUpdatePayload(unitFormFromRow(original), original);
  assert.equal('precision' in unchanged, false, '没改小数位数就不要发这个字段');

  const changed = buildUnitUpdatePayload(
    { ...unitFormFromRow(original), precision: 3 },
    original,
  );
  assert.equal(changed.precision, 3);
});

// ==================== ⑧ DC-09 单位止血：折算单位行加灰字 ====================

test('⑧ DC-09：折算单位返回灰字、基础单位不显示', () => {
  // 文案逐字锁死（与卡面一致：「仅主数据参考，本版本不能在单据中使用」）
  assert.equal(UNIT_DOC_USAGE_HINT, '仅主数据参考，本版本不能在单据中使用');
  assert.equal(unitDocUsageHint(false), UNIT_DOC_USAGE_HINT, '折算单位（is_base = false）必须有灰字');
  assert.equal(unitDocUsageHint(true), '', '基础单位不显示灰字（正常能用的行不该被提示）');
  // 灰字不能替代 tag：折算单位仍然要能分清「折算单位 / 基础单位」
  assert.equal(unitBaseLabel(false), '折算单位');
  assert.equal(unitBaseLabel(true), '基础单位');
});
