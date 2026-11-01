/**
 * `form-field-labels.ts` 单测（`tsx --test`，零新依赖）。
 *
 * 卡面「五、单测 ≥10 条」逐条落地，另加几条真实数据的边界（生产库 2026-11-01 的 `form_data` 实测形状）：
 *
 * | # | 用例 | 断言 |
 * |---|---|---|
 * | ① | 已知字段 | `supplier_id` → 「供应商」 |
 * | ② | 未知字段 | 降级显示原键名（不丢信息） |
 * | ③ | 嵌套字段 | `form.amount` 能展开 |
 * | ④ | ID 查到名称 | 「供应商：示例供应商（种子）」 |
 * | ⑤ | ID 查不到 | 「供应商：(ID: 01a18def…)」 |
 * | ⑥ | 空值 | 「—」 |
 * | ⑦ | 金额 | `226` / `"226.0000"` → `¥226.00`；千分位 |
 * | ⑧ | 枚举 | `EXCL` → 「不含税」；`SUBMITTED` → 「已提交」 |
 * | ⑨ | 日期 | `2026-10-31` 原样 |
 * | ⑩ | 数组明细 | 渲染成表格行/列，不是 JSON |
 * | ⑪ | 数量 | `"100.0000"` → `100`（**不能**变 `¥100.00`） |
 * | ⑫ | 布尔 | 是 / 否 |
 * | ⑬ | 税率 | `tax_rate: 13` 不进金额分支 |
 * | ⑭ | 摘要 | 卡片一行摘要 |
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  EMPTY_PLACEHOLDER,
  buildFormFields,
  buildFormItemTables,
  buildItemTable,
  fieldLabel,
  formSummary,
  formatFieldValue,
  formatMoney,
  formatQty,
  isItemRows,
  type FormField,
  type FormNameLookup,
} from '../../src/views/business/flow/form-field-labels.ts';
import { readItemSnapshot } from '../../src/views/business/flow/flow-business-items.ts';

/** 生产库实测的采购审批 `form_data`（`biz_flow_instance` 2026-11-01，PO-20261031-000003） */
const PURCHASE_FORM = {
  amount: 226.0,
  order_id: '01a18dee-ac34-71c8-80c3-e95d52edf68f',
  order_no: 'PO-20261031-000003',
  create_by: '01a18bc8-bd0c-7792-9118-90fccc9ef0a2',
  total_qty: '100.0000',
  price_mode: 'EXCL',
  supplier_id: 'd1370005-0000-4000-8000-000000000001',
  total_amount: '200.0000',
  warehouse_id: 'd1370001-0000-4000-8000-000000000002',
  total_with_tax: '226.0000',
};

/**
 * 真库实测的**新**采购审批 `form_data`（P1-FIX 后端 `dbc99c3` 起，PO-20261102-000028）。
 *
 * 与 {@link PURCHASE_FORM} 的差别就是后端多冻结了 `supplier_name` / `warehouse_name` / `items`：
 * 这三样让「没有 `business:erp:*` 权限的审批人」也能看懂单据。
 */
const PURCHASE_FORM_SNAPSHOT = {
  amount: 9040.0,
  create_by: '01a19806-ddd6-75ed-b76d-37cd61f1da94',
  items: [
    {
      amount: '8000.0000',
      line_no: 1,
      material_code: 'P1FIX-M-1',
      material_name: 'P1FIX 演示物料',
      price: '80.000000',
      qty: '100.0000',
      tax_rate: '13.0000',
      unit_name: '箱',
    },
  ],
  order_id: '01a19811-fa7e-7247-9c49-758431a2f7a1',
  order_no: 'PO-20261102-000028',
  price_mode: 'EXCL',
  supplier_id: '01a19810-71f7-74d8-beeb-63664a067c8f',
  supplier_name: 'P1FIX 演示供应商',
  total_amount: '8000.0000',
  total_qty: '100.0000',
  total_with_tax: '9040.0000',
  warehouse_id: '01a19810-726a-77e6-a0d1-fd5a8305edbe',
  warehouse_name: 'P1FIX 演示仓库',
};

/** 假的译名函数：只认这张表（模拟「主数据查到了」） */
const lookupKnown: FormNameLookup = (key, id) => {
  const table: Record<string, string> = {
    'supplier_id:d1370005-0000-4000-8000-000000000001': '示例供应商（种子）',
    'warehouse_id:d1370001-0000-4000-8000-000000000002': '分仓（种子）',
    'material_id:m-1': '测试物料',
    'unit_id:u-1': '件',
    // 用户表：给了名字 → 制单人照常显示；不给（审批人 403 的真实情况）→ 整行不显示
    'create_by:01a18bc8-bd0c-7792-9118-90fccc9ef0a2': '管理员',
    'create_by:01a19806-ddd6-75ed-b76d-37cd61f1da94': '管理员',
  };
  return table[`${key}:${id}`];
};

/** 字段列表按 key 取一条（找不到直接抛错，避免「undefined.display」这种假红） */
function fieldOf(fields: FormField[], key: string): FormField {
  const found = fields.find((field) => field.key === key);
  if (!found) {
    throw new Error(`字段列表里应有 ${key}（实际：${fields.map((f) => f.key).join(',')}）`);
  }
  return found;
}

// ==================== ① 已知字段 → 中文标签 ====================

test('① 已知字段 → 中文标签（supplier_id / total_with_tax / price_mode）', () => {
  assert.equal(fieldLabel('supplier_id'), '供应商');
  assert.equal(fieldLabel('total_with_tax'), '价税合计');
  assert.equal(fieldLabel('price_mode'), '金额口径');
  assert.equal(fieldLabel('in_no'), '收货单号');
  assert.equal(fieldLabel('to_warehouse_id'), '调入仓库');
});

// ==================== ② 未知字段 → 降级显示原键名 ====================

test('② 未知字段 → 降级显示原键名（不丢信息）', () => {
  assert.equal(fieldLabel('my_custom_field'), 'my_custom_field');
  assert.equal(fieldLabel(''), '');
  const fields = buildFormFields({ my_custom_field: 'v', another_one: 1 });
  assert.deepEqual(
    fields.map((field) => [field.key, field.label, field.display]),
    [
      ['my_custom_field', 'my_custom_field', 'v'],
      ['another_one', 'another_one', '1'],
    ],
  );
});

// ==================== ③ 嵌套字段 → 能正常展开 ====================

test('③ 嵌套字段（form.xxx）→ 展开成 form.amount 且标签取叶子键的中文', () => {
  const fields = buildFormFields({
    form: {
      amount: '12.5000',
      supplier_id: 'd1370005-0000-4000-8000-000000000001',
      unknown_thing: 1,
    },
    remark: '顶层字段',
  });
  assert.deepEqual(
    fields.map((field) => field.key),
    ['form.supplier_id', 'form.amount', 'remark', 'form.unknown_thing'],
  );
  // 叶子键已知 → 用中文标签
  assert.equal(fieldOf(fields, 'form.amount').label, '金额');
  assert.equal(fieldOf(fields, 'form.amount').display, '¥12.50');
  assert.equal(fieldOf(fields, 'form.supplier_id').label, '供应商');
  // 叶子键未知 → 原键名（带路径，不丢信息）
  assert.equal(fieldOf(fields, 'form.unknown_thing').label, 'form.unknown_thing');
});

// ==================== ④ ID 查到名称 ====================

test('④ supplier_id 查到名称 → 「供应商：示例供应商（种子）」', () => {
  const fields = buildFormFields(PURCHASE_FORM, lookupKnown);
  const supplier = fieldOf(fields, 'supplier_id');
  assert.equal(supplier.label, '供应商');
  assert.equal(supplier.display, '示例供应商（种子）');
  // 仓库也译名
  assert.equal(fieldOf(fields, 'warehouse_id').display, '分仓（种子）');
  // 制单人走 user 表（这里没造 → 兜底 ID）
  assert.equal(fieldOf(fields, 'create_by').label, '制单人');
});

// ==================== ⑤ ID 查不到 → 显示前 8 位 ====================

test('⑤ 查不到名称 → 显示 (ID: 前 8 位…)（不许空白、不许整条 uuid）', () => {
  const fields = buildFormFields(PURCHASE_FORM); // 不传 lookup = 主数据没拉到
  const supplier = fieldOf(fields, 'supplier_id');
  assert.equal(supplier.display, '(ID: d1370005…)');
  assert.equal(supplier.empty, false);
  // 完全未知的 uuid 字段也走同一兜底
  assert.equal(
    formatFieldValue('some_ref', '01a18def-1a2b-7329-a9d3-c6afa9e04220'),
    '(ID: 01a18def…)',
  );
});

// ==================== ⑥ 空值 → — ====================

test('⑥ 空值（null / undefined / 空串）→ 「—」', () => {
  assert.equal(formatFieldValue('remark', null), EMPTY_PLACEHOLDER);
  assert.equal(formatFieldValue('remark', undefined), EMPTY_PLACEHOLDER);
  assert.equal(formatFieldValue('remark', '   '), EMPTY_PLACEHOLDER);
  const fields = buildFormFields({ remark: null, order_no: 'PO-1' }, lookupKnown);
  assert.equal(fieldOf(fields, 'remark').display, EMPTY_PLACEHOLDER);
  assert.equal(fieldOf(fields, 'remark').empty, true);
  assert.equal(fieldOf(fields, 'order_no').empty, false);
});

// ==================== ⑦ 金额 → ¥226.00 ====================

test('⑦ 金额：数字 226 与数字字符串 "226.0000" 都是 ¥226.00，且带千分位', () => {
  assert.equal(formatMoney(226), '¥226.00');
  assert.equal(formatMoney('226.0000'), '¥226.00');
  assert.equal(formatMoney(1234567.891), '¥1,234,567.89');
  assert.equal(formatMoney(-12.5), '¥-12.50');
  // 非数字 → null（由调用方降级原值，不硬造 0）
  assert.equal(formatMoney('EXCL'), null);

  const fields = buildFormFields(PURCHASE_FORM, lookupKnown);
  assert.equal(fieldOf(fields, 'total_with_tax').label, '价税合计');
  assert.equal(fieldOf(fields, 'total_with_tax').display, '¥226.00');
  assert.equal(fieldOf(fields, 'total_amount').display, '¥200.00');
  // 采购单的 `amount` 与 `total_with_tax` 同值 → 不重复显示（见 ⑯）
  assert.ok(!fields.some((field) => field.key === 'amount'));
});

// ==================== ⑧ 枚举 → 中文 ====================

test('⑧ 枚举：EXCL → 不含税；SUBMITTED → 已提交；未知枚举原样', () => {
  assert.equal(formatFieldValue('price_mode', 'EXCL'), '不含税');
  assert.equal(formatFieldValue('price_mode', 'INCL'), '含税');
  assert.equal(formatFieldValue('status', 'SUBMITTED'), '已提交');
  assert.equal(formatFieldValue('in_type', 'PURCHASE'), '采购收货');
  // 未知枚举值不猜（原样显示大写值）
  assert.equal(formatFieldValue('price_mode', 'WHATEVER'), 'WHATEVER');
  const fields = buildFormFields(PURCHASE_FORM, lookupKnown);
  assert.equal(fieldOf(fields, 'price_mode').display, '不含税');
});

// ==================== ⑨ 日期 → 原样 ====================

test('⑨ 日期 2026-10-31 → 原样（已可读，不重排、不加时区）', () => {
  assert.equal(formatFieldValue('order_date', '2026-10-31'), '2026-10-31');
  assert.equal(formatFieldValue('pay_date', '2026-10-31'), '2026-10-31');
  const fields = buildFormFields({ order_date: '2026-10-31' });
  assert.equal(fieldOf(fields, 'order_date').display, '2026-10-31');
});

// ==================== ⑩ 数组 items → 表格（不是 JSON） ====================

test('⑩ 数组明细 → 表格（3 行；列标题中文；单元格格式化；不是 JSON 字符串）', () => {
  const items = [
    { line_no: 1, material_id: 'm-1', unit_id: 'u-1', qty: '100.0000', price: '2.0000', amount: '200.0000' },
    { line_no: 2, material_id: 'm-1', unit_id: 'u-1', qty: '2.0000', price: '10.0000', amount: '20.0000' },
    { line_no: 3, material_id: 'm-1', unit_id: 'u-1', qty: '3.0000', price: '2.0000', amount: '6.0000' },
  ];
  const tables = buildFormItemTables({ order_no: 'PO-1', items }, lookupKnown);
  assert.equal(tables.length, 1);
  const table = tables[0];
  assert.equal(table.key, 'items');
  assert.equal(table.label, '明细');
  assert.equal(table.rows.length, 3);
  // 数量列已带单位 → 不单开「单位」列
  assert.deepEqual(
    table.columns.map((column) => column.title),
    ['行号', '物料', '数量', '单价', '金额'],
  );

  const first = table.rows[0];
  assert.equal(first.c0, '1');
  assert.equal(first.c1, '测试物料');
  assert.equal(first.c2, '100 件'); // 数量带单位
  assert.equal(first.c3, '¥2.00');
  assert.equal(first.c4, '¥200.00');
  // 行 key 给 t-table 用
  assert.equal(first.__rowKey, 'r0');

  // 明细不进「普通字段」列表；且绝不出现 JSON 字符串
  const fields = buildFormFields({ order_no: 'PO-1', items }, lookupKnown);
  assert.deepEqual(
    fields.map((field) => field.key),
    ['order_no'],
  );
  for (const row of table.rows) {
    for (const value of Object.values(row)) {
      assert.ok(!value.includes('{"'), `明细单元格不许是 JSON：${value}`);
    }
  }
});

// ==================== ⑪ 数量 ====================

test('⑪ 数量 "100.0000" → 100（不能变 ¥100.00）；小数保留', () => {
  assert.equal(formatQty('100.0000'), '100');
  assert.equal(formatQty(3.5), '3.5');
  assert.equal(formatQty('12345.6789'), '12,345.6789');
  assert.equal(formatQty('0.0000'), '0');
  const fields = buildFormFields(PURCHASE_FORM, lookupKnown);
  assert.equal(fieldOf(fields, 'total_qty').label, '数量合计');
  assert.equal(fieldOf(fields, 'total_qty').display, '100');
});

// ==================== ⑫ 布尔 ====================

test('⑫ 布尔 → 是 / 否（发票已到这类开关字段）', () => {
  assert.equal(formatFieldValue('invoice_received', true), '是');
  assert.equal(formatFieldValue('invoice_received', false), '否');
});

// ==================== ⑬ 税率不进金额分支 ====================

test('⑬ tax_rate 13 → "13"（比例类字段不许变成 ¥13.00）', () => {
  assert.equal(formatFieldValue('tax_rate', 13), '13');
  assert.equal(formatFieldValue('tax_rate', '13.0000'), '13');
  assert.equal(formatFieldValue('credit_days', 30), '30');
});

// ==================== ⑮ 技术 id 与单号并存 → 只显示单号 ====================

test('⑮ order_id 与 order_no 并存 → 只显示「采购单号」，不显示 (ID: …) 噪声', () => {
  const fields = buildFormFields(PURCHASE_FORM, lookupKnown);
  const keys = fields.map((field) => field.key);
  assert.ok(!keys.includes('order_id'), `不该出现 order_id（实际：${keys.join(',')}）`);
  assert.ok(keys.includes('order_no'));
  // 只有 id、没有单号时照常显示（信息不丢）
  const onlyId = buildFormFields({ order_id: '01a18dee-ac34-71c8-80c3-e95d52edf68f' });
  assert.deepEqual(
    onlyId.map((field) => [field.key, field.display]),
    [['order_id', '(ID: 01a18dee…)']],
  );
});

// ==================== ⑯ 字段顺序 ====================

test('⑯ 字段按业务顺序排（单号 → 供应商 → 仓库 → 金额 → 数量 → 人），未知键排最后', () => {
  const fields = buildFormFields({ ...PURCHASE_FORM, zz_custom: 'x' }, lookupKnown);
  // `amount` 与 `total_with_tax` 同值 → 只留后者（不重复显示「金额」）
  assert.deepEqual(
    fields.map((field) => field.label),
    ['采购单号', '供应商', '收货仓库', '金额口径', '价款', '价税合计', '数量合计', '制单人', 'zz_custom'],
  );
  // 付款单没有 total_with_tax → amount 照常显示
  const payable = buildFormFields({ payment_no: 'PAY-1', amount: 100, amount_text: '100.0000' });
  assert.deepEqual(
    payable.map((field) => [field.label, field.display]),
    [
      ['付款单号', 'PAY-1'],
      ['金额', '¥100.00'],
    ],
  );
});

// ==================== ⑭ 卡片摘要 ====================

test('⑭ 待办卡片摘要：供应商 + 价税合计 + 数量合计（不带 order_id / create_by 噪声）', () => {
  const summary = formSummary(PURCHASE_FORM, lookupKnown);
  assert.equal(summary, '供应商：示例供应商（种子） · 收货仓库：分仓（种子） · 价税合计：¥226.00');
  // 译名没拿到时摘要仍可读（ID 前 8 位），不空白
  const bare = formSummary(PURCHASE_FORM);
  assert.equal(bare, '供应商：(ID: d1370005…) · 收货仓库：(ID: d1370001…) · 价税合计：¥226.00');
  // 自定义流程（没有任何白名单字段）→ 空串，卡片不显示摘要行
  assert.equal(formSummary({ foo: 'bar' }, lookupKnown), '');
  assert.equal(formSummary(null, lookupKnown), '');
});

// ==================== 附加：空数组 / 普通数组 / 单行明细 ====================

test('附加：空数组 → 「—」；字符串数组 → 「、」拼接；单行明细也成表', () => {
  assert.equal(formatFieldValue('tags', []), EMPTY_PLACEHOLDER);
  assert.equal(formatFieldValue('tags', ['甲', '乙']), '甲、乙');
  assert.equal(isItemRows([{ a: 1 }]), true);
  assert.equal(isItemRows([1, 2]), false);
  assert.equal(isItemRows([]), false);

  const single = buildItemTable('items', '明细', [{ material_id: 'm-1', qty: '1' }], lookupKnown);
  assert.equal(single.rows.length, 1);
  assert.deepEqual(
    single.columns.map((column) => column.title),
    ['物料', '数量'],
  );
  assert.equal(single.rows[0].c0, '测试物料');
  assert.equal(single.rows[0].c1, '1');
});

// ==================== ⑰ 新实例：*_name → 中文标签 + *_id 让位 ====================

test('⑰ 新实例（form_data 有 *_name）：5 个名称键有中文标签，且 *_id 让位 —— 页面上不再有 (ID: …) 行', () => {
  // ① 标签表本身（后端 form_snapshot 的键名 = 契约）
  assert.equal(fieldLabel('supplier_name'), '供应商');
  assert.equal(fieldLabel('warehouse_name'), '收货仓库');
  assert.equal(fieldLabel('from_warehouse_name'), '调出仓库');
  assert.equal(fieldLabel('to_warehouse_name'), '调入仓库');
  assert.equal(fieldLabel('unit_name'), '单位');

  // ② 字段列表：**故意不传 lookup**（审批人查不到主数据是常态），名字照样是中文
  const fields = buildFormFields(PURCHASE_FORM_SNAPSHOT);
  assert.deepEqual(
    fields.map((field) => field.label),
    // 制单人不在列表里：没有 lookup（审批人 403 的真实情况）→ 不留一行 (ID: …)
    ['采购单号', '供应商', '收货仓库', '金额口径', '价款', '价税合计', '数量合计'],
  );
  const keys = fields.map((field) => field.key);
  assert.ok(!keys.includes('supplier_id'), `supplier_id 应让位（实际：${keys.join(',')}）`);
  assert.ok(!keys.includes('warehouse_id'), `warehouse_id 应让位（实际：${keys.join(',')}）`);
  assert.equal(fieldOf(fields, 'supplier_name').display, 'P1FIX 演示供应商');
  assert.equal(fieldOf(fields, 'warehouse_name').display, 'P1FIX 演示仓库');
  // 卡面验收的「页面上不再出现 (ID: …) 行」：整块字段区都不许有兜底串
  for (const field of fields) {
    assert.ok(
      !field.display.includes('(ID:'),
      `字段 ${field.key} 不该是 ID（实际：${field.display}）`,
    );
  }
  assert.equal(fieldOf(fields, 'supplier_name').display, 'P1FIX 演示供应商');
  // 「供应商」只出现一次（id 让位后不重复）
  assert.equal(fields.filter((field) => field.label === '供应商').length, 1);
});

test('⑰b 调拨：调出 / 调入仓库只显示中文名（from_/to_warehouse_id 让位）', () => {
  const fields = buildFormFields({
    transfer_id: '01a19811-0000-7000-8000-000000000001',
    transfer_no: 'TR-20261102-000001',
    from_warehouse_id: '01a19811-0000-7000-8000-0000000000a1',
    to_warehouse_id: '01a19811-0000-7000-8000-0000000000b2',
    from_warehouse_name: '一号原料仓',
    to_warehouse_name: '二号成品仓',
    total_qty: '20.0000',
  });
  assert.deepEqual(
    fields.map((field) => [field.label, field.display]),
    [
      ['调拨单号', 'TR-20261102-000001'],
      ['调出仓库', '一号原料仓'],
      ['调入仓库', '二号成品仓'],
      ['数量合计', '20'],
    ],
  );
});

test('⑰c 制单人：译名拿不到 → 整行不显示（不留裸 ID）；拿到了 → 照常显示姓名', () => {
  // 审批人真实处境：没有 system:user:list → 查不到姓名
  const hidden = buildFormFields(PURCHASE_FORM_SNAPSHOT);
  assert.ok(!hidden.some((field) => field.key === 'create_by'));

  // 有权限的人：译名照常显示，什么都不丢
  const shown = buildFormFields(PURCHASE_FORM_SNAPSHOT, lookupKnown);
  assert.equal(fieldOf(shown, 'create_by').label, '制单人');
  assert.equal(fieldOf(shown, 'create_by').display, '管理员');

  // 硬红线 3 不变：**别的**未知 uuid 字段仍然兜底显示前 8 位（不许藏信息）
  assert.equal(
    formatFieldValue('some_ref', '01a18def-1a2b-7329-a9d3-c6afa9e04220'),
    '(ID: 01a18def…)',
  );
});

// ==================== ⑱ 藏 id 的前提：名字必须非空 ====================

test('⑱ 名字为空 / 缺键 / 空串 → *_id 照常显示（不许把信息藏没）', () => {
  const idOnly = buildFormFields({ supplier_id: '01a19810-71f7-74d8-beeb-63664a067c8f' });
  assert.deepEqual(
    idOnly.map((field) => [field.key, field.label, field.display]),
    [['supplier_id', '供应商', '(ID: 01a19810…)']],
  );

  const blankName = buildFormFields({
    supplier_id: '01a19810-71f7-74d8-beeb-63664a067c8f',
    supplier_name: '   ',
  });
  assert.ok(blankName.some((field) => field.key === 'supplier_id'));

  const nullName = buildFormFields({
    warehouse_id: '01a19810-726a-77e6-a0d1-fd5a8305edbe',
    warehouse_name: null,
  });
  assert.ok(nullName.some((field) => field.key === 'warehouse_id'));

  // 嵌套形状（`form.supplier_id`）也认同一父级的名字
  const nested = buildFormFields({
    form: { supplier_id: '01a19810-71f7-74d8-beeb-63664a067c8f', supplier_name: '嵌套供应商' },
  });
  assert.deepEqual(
    nested.map((field) => [field.key, field.label, field.display]),
    [['form.supplier_name', '供应商', '嵌套供应商']],
  );
});

// ==================== ⑲ 摘要白名单（名称键） ====================

test('⑲ 待办卡片摘要：新实例显示中文名（不是 (ID: …)）；老实例口径不变', () => {
  const summary = formSummary(PURCHASE_FORM_SNAPSHOT);
  // 卡面验收要的两段都在，且没有任何 (ID: …)
  assert.ok(summary.includes('供应商：P1FIX 演示供应商'), summary);
  assert.ok(summary.includes('价税合计：¥9,040.00'), summary);
  assert.ok(!summary.includes('(ID:'), summary);
  assert.equal(summary, '供应商：P1FIX 演示供应商 · 收货仓库：P1FIX 演示仓库 · 价税合计：¥9,040.00');

  // 调拨：调出 → 调入（名称键在对应 _id 前）
  assert.equal(
    formSummary({ from_warehouse_name: '一号原料仓', to_warehouse_name: '二号成品仓' }),
    '调出仓库：一号原料仓 · 调入仓库：二号成品仓',
  );

  // 老实例（无 *_name）：仍走 id 兜底（⑭ 已钉住带译名的形状，这里钉「不空白、不崩」）
  assert.equal(
    formSummary(PURCHASE_FORM),
    '供应商：(ID: d1370005…) · 收货仓库：(ID: d1370001…) · 价税合计：¥226.00',
  );
});

// ==================== ⑳ 明细读 form_data.items 快照 ====================

test('⑳ 快照明细（8 键）→ 表头全中文（含「单位」），数值仍是 100 / ¥80.00 / 13', () => {
  const tables = buildFormItemTables(PURCHASE_FORM_SNAPSHOT);
  assert.equal(tables.length, 1);
  const table = tables[0];
  assert.equal(table.key, 'items');
  assert.equal(table.label, '明细');
  assert.equal(table.rows.length, 1);
  assert.deepEqual(
    table.columns.map((column) => column.title),
    ['行号', '物料编码', '物料名称', '数量', '单位', '单价', '税率(%)', '金额'],
  );
  assert.deepEqual(Object.values(table.rows[0]).filter((value) => value !== 'r0'), [
    '1',
    'P1FIX-M-1',
    'P1FIX 演示物料',
    '100',
    '箱',
    '¥80.00',
    '13',
    '¥8,000.00',
  ]);
  // 明细不进「普通字段」列表（不会出现「明细 1 行」那一行）
  const fields = buildFormFields(PURCHASE_FORM_SNAPSHOT);
  assert.ok(!fields.some((field) => field.key === 'items'));
});

test('⑳b items 为空数组 → 不渲染表格（按「无明细」处理，不报错）', () => {
  assert.deepEqual(buildFormItemTables({ ...PURCHASE_FORM_SNAPSHOT, items: [] }), []);
  assert.deepEqual(buildFormItemTables({ order_no: 'PO-1' }), []);
  assert.deepEqual(buildFormItemTables(null), []);
});

// ==================== ㉑ 老实例降级：readItemSnapshot ====================

test('㉑ readItemSnapshot：老实例（无 items）→ legacy，空数组 / 自定义流程 → 不报 legacy', () => {
  // 老实例：有明细的业务类型，但快照里没有 items
  const legacy = readItemSnapshot('PO_APPROVE', { order_no: 'PO-20261031-000003' });
  assert.deepEqual(legacy.rows, []);
  assert.equal(legacy.legacy, true);
  assert.equal(readItemSnapshot('STOCK_IN_POST', {}).legacy, true);
  assert.equal(readItemSnapshot('STOCK_TRANSFER', undefined).legacy, true);

  // 新实例：行原样给出（顺序 = 后端 line_no 升序，前端不重排）
  const fresh = readItemSnapshot('PO_APPROVE', PURCHASE_FORM_SNAPSHOT);
  assert.equal(fresh.legacy, false);
  assert.equal(fresh.rows.length, 1);
  assert.equal(fresh.rows[0].material_name, 'P1FIX 演示物料');

  // items 明确为空数组 → 「无明细」，不谎报老实例
  const empty = readItemSnapshot('PO_APPROVE', { items: [] });
  assert.deepEqual(empty, { rows: [], legacy: false });

  // 没有 items 的自定义流程 → 不提示（它本来就没明细可言）
  assert.deepEqual(readItemSnapshot('CUSTOM_FLOW', { foo: 'bar' }), { rows: [], legacy: false });

  // 脏数据（非空数组但不是对象行）→ 按老实例降级，页面显示 —
  assert.deepEqual(readItemSnapshot('PO_APPROVE', { items: [1, 2] }), { rows: [], legacy: true });
});
