/**
 * A6 单测：采购单（采购类型 / 合同选择器 / 供应商只读 / 列表快照列）+ 入库单（供应商只读 / 无源标记 / 采购单选择）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零 DOM 依赖）。
 *
 * # 为什么这么测（本仓既有姿势：逻辑测纯函数 + 接线测源码）
 *
 * 本仓没有 happy-dom/jsdom，`tests/unit` 里也没有渲染 `.vue` 的先例；而这次要钉住的恰好是
 * 两类**渲染错就出错账**的东西：
 *
 * 1. **口径**（纯函数）：合同采购能不能提交、供应商到底写谁的、请求体该带 / 不该带哪些字段；
 * 2. **接线**（源码断言）：字段该出现 / 该只读 / 该消失，以及「列表合同号只能读快照列」
 *    「原有审批单 FLW-xxx 链接不许丢」——这三条 `vue-tsc` 抓不到、e2e 又只看得见一张正常单。
 *
 * # 与后端对齐的硬口径（不是抄卡面）
 *
 * - 合同可选状态 = `contract_model::USABLE_FOR_ORDER`（`SIGNED` / `ACTIVE`），**不是**卡面写的
 *   「只 ACTIVE」—— 下面有一条用例直接读 Rust 源码比对；
 * - 入库单「无源」= `is_sourceless`（V0199），且无源必须带备注（后端 400 `66029`）；
 * - 有源入库单**不传 `supplier_id`**（A4：服务端从采购单派生，传了不一致就 400）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

import {
  ORDERABLE_CONTRACT_STATUSES,
  PURCHASE_TYPE_CONTRACT,
  PURCHASE_TYPE_SPOT,
  applyContractToForm,
  applyPurchaseType,
  buildPurchasePayload,
  detailToForm,
  emptyPurchaseForm,
  isContractLocked,
  isContractPurchase,
  isPurchaseTypeLocked,
  purchaseTypeLabel,
  selectableContracts,
  supplierHintOf,
  validatePurchaseForm,
} from '@/views/business/erp/purchase/purchase-logic';
import {
  RECEIVABLE_ORDER_STATUSES,
  applySourceMode,
  buildStockInPayload,
  detailToForm as stockInDetailToForm,
  emptyStockInForm,
  isSupplierReadonly,
  purchaseOrderOptionLabel,
  selectablePurchaseOrders,
  showsSourcelessMark,
  supplierDisplayText,
  supplierHintText,
  validateStockInForm,
} from '@/views/business/erp/stockin/stockin-logic';
import type {
  ContractRow,
  PurchaseOrderRow,
  StockInRow,
} from '@/api/business/erp';

// ---------------------------------------------------------------- 夹具

/** 读后端源码（口径的唯一真相；前端跟丢了这里先红） */
function backendSource(relative: string): string {
  return readFileSync(new URL(`../../../backend/src/business/${relative}`, import.meta.url), 'utf8');
}

/** 读前端源码（接线断言） */
function frontendSource(relative: string): string {
  return readFileSync(new URL(`../../src/${relative}`, import.meta.url), 'utf8');
}

/**
 * 截出「从 `start` 到其后第一处 `end`」的片段。
 *
 * 模板断言要在**某个字段块内部**判断（不然 `v-if` 可能来自隔壁字段），
 * 直接按字符距离写 `[\s\S]{0,600}` 既脆又读不懂。
 */
function blockAround(source: string, start: string, end: string): string {
  const from = source.indexOf(start);
  assert.notEqual(from, -1, `找不到片段起点：${start}`);
  const to = source.indexOf(end, from);
  assert.notEqual(to, -1, `找不到片段终点：${end}`);
  return source.slice(from, to + end.length);
}

const PURCHASE_VIEW = 'views/business/erp/purchase/index.vue';
const STOCKIN_VIEW = 'views/business/erp/stockin/index.vue';

/** 一张可下单的采购类合同 */
function contractRow(overrides: Partial<ContractRow> = {}): ContractRow {
  return {
    id: '018f4b2c-1111-7222-8333-444455556666',
    contract_no: 'HT-20261102-000001',
    name: '2026 年度钢材框架合同',
    contract_name: '2026 年度钢材框架合同',
    contract_type: 'PURCHASE',
    party_b_type: 'SUPPLIER',
    party_b_id: '018f4b2c-2222-7222-8333-444455556666',
    party_b_name: '北京钢材有限公司',
    party_b_current_name: '北京钢材有限公司',
    amount: '1000000.0000',
    currency_code: 'CNY',
    sign_date: '2026-01-01',
    start_date: '2026-01-01',
    end_date: '2026-12-31',
    status: 'ACTIVE',
    status_label: '已生效',
    updated_at: '2026-11-02T10:00:00Z',
    ...overrides,
  };
}

/** 一行采购单（只覆盖断言用到的列） */
function purchaseRow(overrides: Partial<PurchaseOrderRow> = {}): PurchaseOrderRow {
  return {
    id: '018f4b2c-3333-7222-8333-444455556666',
    order_no: 'PO-20261102-000001',
    supplier_id: '018f4b2c-2222-7222-8333-444455556666',
    order_date: '2026-11-02',
    expect_date: null,
    warehouse_id: '018f4b2c-4444-7222-8333-444455556666',
    status: 'DRAFT',
    currency_code: 'CNY',
    total_qty: '10.0000',
    total_amount: '200.00',
    total_tax: '26.00',
    total_with_tax: '226.00',
    credit_days: 0,
    remark: null,
    price_mode: 'EXCL',
    purchase_type: 'CONTRACT',
    contract_id: '018f4b2c-1111-7222-8333-444455556666',
    contract_no: 'HT-20261102-000001',
    contract_no_snapshot: 'HT-20261102-000001',
    supplier_name: '北京钢材有限公司',
    supplier_name_snapshot: '北京钢材有限公司',
    submitted_at: null,
    submitted_by: null,
    approved_at: null,
    approved_by: null,
    closed_at: null,
    closed_by: null,
    approval_instance_id: '018f4b2c-5555-7222-8333-444455556666',
    approval_instance_no: 'FLW-20261102-000001',
    created_at: '2026-11-02T10:00:00Z',
    updated_at: '2026-11-02T10:00:00Z',
    ...overrides,
  };
}

/** 一张收货单（只覆盖断言用到的列） */
function stockInRow(overrides: Partial<StockInRow> = {}): StockInRow {
  return {
    id: '018f4b2c-6666-7222-8333-444455556666',
    in_no: 'IN-20261102-000001',
    in_type: 'PURCHASE',
    source_order_id: null,
    source_order_no: null,
    supplier_id: null,
    supplier_name: null,
    is_sourceless: false,
    warehouse_id: '018f4b2c-4444-7222-8333-444455556666',
    in_date: '2026-11-02',
    status: 'DRAFT',
    status_label: '草稿',
    currency_code: 'CNY',
    total_qty: '1.0000',
    total_amount: '10.00',
    total_tax: '1.30',
    total_with_tax: '11.30',
    invoice_received: true,
    voucher_id: null,
    payable_id: null,
    remark: null,
    posted_at: null,
    posted_by: null,
    cancelled_at: null,
    cancelled_by: null,
    cancel_reason: null,
    approval_instance_id: null,
    create_by: null,
    created_at: '2026-11-02T10:00:00Z',
    updated_at: '2026-11-02T10:00:00Z',
    ...overrides,
  };
}

// ---------------------------------------------------------------- ① 采购类型 → 字段形态

test('④ 采购类型选 CONTRACT：合同选择器出现、供应商变只读（模板接线 + 逻辑判据）', () => {
  const source = frontendSource(PURCHASE_VIEW);

  // 逻辑层：合同采购 = 供应商只读
  const form = emptyPurchaseForm();
  assert.equal(form.purchase_type, PURCHASE_TYPE_CONTRACT, '新建默认必须是合同采购（卡面口径）');
  assert.equal(isContractPurchase(form), true);
  assert.equal(supplierHintOf(form), '供应商来自合同乙方，如需更换请改合同');

  // 模板层：合同选择器只在合同采购时渲染；供应商在合同采购时走只读输入框
  assert.match(
    source,
    /<t-form-item\s+v-if="isContractPurchase\(form\)"\s+label="合同"/,
    '合同字段必须挂在「合同采购」条件下',
  );
  assert.match(blockAround(source, 'label="合同"', '</t-form-item>'), /data-testid="purchase-contract"/);
  const supplierBlock = blockAround(source, 'label="供应商"', '</t-form-item>');
  assert.match(supplierBlock, /v-if="isContractPurchase\(form\)"[\s\S]*data-testid="purchase-supplier-readonly"/);
  // 只读是真只读（readonly 属性 + 值来自合同乙方，不是「看起来像输入框的可编辑框」）
  assert.match(source, /:value="contractSupplierText"[\s\S]{0,80}readonly/);
});

test('⑤ 采购类型选 SPOT：合同选择器不渲染、供应商可编辑', () => {
  const source = frontendSource(PURCHASE_VIEW);
  const form = emptyPurchaseForm();
  applyPurchaseType(form, PURCHASE_TYPE_SPOT);

  assert.equal(isContractPurchase(form), false);
  assert.equal(supplierHintOf(form), '', '现货采购不该挂「来自合同乙方」的说明');

  // 模板层：现货采购分支是可选的下拉（v-else 挂在 select 上，没有 readonly）
  assert.match(source, /<t-select\s+v-else[\s\S]{0,200}data-testid="purchase-supplier"/);
  // 切换类型会清掉另一条路的字段（否则会带着 contract_id 提交 → 后端 400）
  const contractForm = emptyPurchaseForm();
  applyContractToForm(contractForm, contractRow());
  applyPurchaseType(contractForm, PURCHASE_TYPE_SPOT);
  assert.equal(contractForm.contract_id, '');
  assert.equal(contractForm.contract_no, '');
  assert.equal(contractForm.contract_supplier_name, '');
});

test('⑥ CONTRACT 型未选合同 → 提交被拦（且不误报「请选择供应商」）', () => {
  const form = emptyPurchaseForm(); // 默认 CONTRACT，未选合同
  const errors = validatePurchaseForm(form);
  assert.ok(errors.contract_id, '必须拦下「合同采购未选合同」');
  assert.equal(errors.supplier_id, undefined, '供应商是合同带出的结果，不该再报「请选择供应商」');
  // 现货采购反过来：拦供应商、不拦合同
  applyPurchaseType(form, PURCHASE_TYPE_SPOT);
  const spotErrors = validatePurchaseForm(form);
  assert.ok(spotErrors.supplier_id);
  assert.equal(spotErrors.contract_id, undefined);
});

test('⑦ 选中合同 → 供应商字段被自动填入乙方名（id + 名称一把写）', () => {
  const form = emptyPurchaseForm();
  applyContractToForm(form, contractRow());

  assert.equal(form.contract_id, '018f4b2c-1111-7222-8333-444455556666');
  assert.equal(form.contract_no, 'HT-20261102-000001', '合同号要在表单上可见（让操作员确认选对了）');
  assert.equal(form.supplier_id, '018f4b2c-2222-7222-8333-444455556666');
  assert.equal(form.contract_supplier_name, '北京钢材有限公司', '供应商字段显示的就是乙方名');
  // 乙方主档改名后以「现值」为准，主档删了才回落到签约快照
  const renamed = emptyPurchaseForm();
  applyContractToForm(renamed, contractRow({ party_b_current_name: '北京钢材（集团）有限公司' }));
  assert.equal(renamed.contract_supplier_name, '北京钢材（集团）有限公司');
  const deleted = emptyPurchaseForm();
  applyContractToForm(deleted, contractRow({ party_b_current_name: null }));
  assert.equal(deleted.contract_supplier_name, '北京钢材有限公司');
});

test('⑧ 编辑 CONTRACT 型单：采购类型只读、合同只读（不给换合同）', () => {
  const source = frontendSource(PURCHASE_VIEW);
  const editing = detailToForm(purchaseRow(), []);

  assert.equal(editing.purchase_type, PURCHASE_TYPE_CONTRACT, '回填要认库里的类型');
  assert.equal(editing.contract_no, 'HT-20261102-000001');
  assert.equal(isPurchaseTypeLocked(editing), true, '编辑态采购类型必须只读（后端改类型 409）');
  assert.equal(isContractLocked(editing), true, '编辑态合同必须只读（避免洗掉关联）');
  // 新建态两者都可动
  const creating = emptyPurchaseForm();
  assert.equal(isPurchaseTypeLocked(creating), false);
  assert.equal(isContractLocked(creating), false);
  // 模板层：单选组与合同输入框都吃这两个判据
  assert.match(source, /:disabled="isPurchaseTypeLocked\(form\)"/);
  assert.match(source, /v-if="isContractLocked\(form\)"[\s\S]{0,200}data-testid="purchase-contract-readonly"/);
  // 现货采购的存量单：类型只读但不该出现「合同只读框」
  const spotEditing = detailToForm(
    purchaseRow({ purchase_type: 'SPOT', contract_id: null, contract_no: null, contract_no_snapshot: null }),
    [],
  );
  assert.equal(spotEditing.purchase_type, PURCHASE_TYPE_SPOT);
  assert.equal(isContractLocked(spotEditing), false);
});

// ---------------------------------------------------------------- ② 列表 / 详情

test('⑨ 列表显示「合同采购 / 现货采购」文案正确（且走同一个函数，不在模板里写三元）', () => {
  const source = frontendSource(PURCHASE_VIEW);

  assert.equal(purchaseTypeLabel('CONTRACT'), '合同采购');
  assert.equal(purchaseTypeLabel('SPOT'), '现货采购');
  // 老数据（后端未回填 / 字段缺失）按现货采购显示，不显示空白
  assert.equal(purchaseTypeLabel(null), '现货采购');

  assert.match(source, /colKey: 'purchase_type', title: '采购类型'/);
  assert.match(source, /#purchase_type="\{ row \}"[\s\S]{0,400}purchaseTypeLabel\(row\.purchase_type\)/);
  // 模板里不许再出现「合同采购」硬编码文案（除单选项外），否则两处口径会漂
  const templatePart = source.slice(0, source.indexOf('<script setup'));
  assert.doesNotMatch(templatePart, /purchase_type === 'CONTRACT'/);
});

test('⑩ 列表合同号读快照字段 contract_no_snapshot（不是实时 JOIN / 不是 contract_id）', () => {
  const source = frontendSource(PURCHASE_VIEW);
  const templatePart = source.slice(0, source.indexOf('<script setup'));

  assert.match(source, /colKey: 'contract_no_snapshot', title: '合同号'/);
  assert.match(templatePart, /#contract_no_snapshot="\{ row \}"[\s\S]{0,300}row\.contract_no_snapshot/);
  // 列表渲染不许出现「合同号读 contract_no（detail 才有）/ 拿 contract_id 反查」的写法
  assert.doesNotMatch(templatePart, /row\.contract_no\b(?!_snapshot)/);
  // 列表页不该为合同号再发请求（合同接口只在建单表单下拉里用一次）
  assert.equal((source.match(/fetchContractPage\(/g) ?? []).length, 1, '合同接口只该在 loadBasics 用一次');
});

test('⑭ 原有「审批单 FLW-xxx」链接仍然存在且可点（这次改动不许弄丢）', () => {
  const source = frontendSource(PURCHASE_VIEW);

  // 详情头部的审批链接（原有行为）
  assert.match(source, /data-testid="purchase-detail-approval"[\s\S]{0,200}@click="openApprovalInstance\(detail\.approval_instance_id\)"/);
  assert.match(source, /\{\{ detail\.approval_instance_no \?\? detail\.approval_instance_id \}\}/);
  // 审批进度区块（P1-FIX-3 挪到最底部的那块）仍在抽屉里
  assert.match(source, /data-testid="purchase-detail-approval-progress"/);
  // 新加的合同号链接是**另一个** testid，两条链接并存
  assert.match(source, /data-testid="purchase-detail-contract-link"/);
  assert.notEqual(
    source.indexOf('data-testid="purchase-detail-approval"'),
    source.indexOf('data-testid="purchase-detail-contract-link"'),
    '合同号链接与审批单链接必须是两个独立元素',
  );
  // 详情里的采购类型 / 合同号两项都在基本信息区
  assert.match(source, /data-testid="purchase-detail-type"/);
});

test('合同下拉只列「采购类 + 可下单状态」，状态口径与后端 USABLE_FOR_ORDER 逐字一致', () => {
  const rows = [
    contractRow({ id: 'a', status: 'ACTIVE' }),
    contractRow({ id: 'b', status: 'SIGNED' }),
    contractRow({ id: 'c', status: 'DRAFT' }),
    contractRow({ id: 'd', status: 'COMPLETED' }),
    contractRow({ id: 'e', status: 'TERMINATED' }),
    contractRow({ id: 'f', status: 'ACTIVE', contract_type: 'SALES' }),
  ];
  assert.deepEqual(
    selectableContracts(rows).map((row) => row.id),
    ['a', 'b'],
    '只留 PURCHASE + SIGNED/ACTIVE',
  );

  // 与后端常量对齐（后端加/减一个状态，这里必须先红）
  const backend = backendSource('erp/contract_model.rs');
  const match = /pub const USABLE_FOR_ORDER[^=]*=\s*\[([^\]]+)\];/.exec(backend);
  assert.ok(match, '后端 USABLE_FOR_ORDER 常量找不到（被改名了？）');
  const backendValues = match[1]
    .split(',')
    .map((item) => item.trim())
    .filter(Boolean)
    .map((item) => (/STATUS_([A-Z_]+)/.exec(item)?.[1] ?? item.replace(/'/g, '')));
  assert.deepEqual([...ORDERABLE_CONTRACT_STATUSES].sort(), [...backendValues].sort());
});

test('请求体口径：合同采购不发 supplier_id / 现货采购不带 contract_id', () => {
  const contractForm = emptyPurchaseForm();
  applyContractToForm(contractForm, contractRow());
  const contractPayload = buildPurchasePayload(contractForm);
  assert.equal(contractPayload.purchase_type, 'CONTRACT');
  assert.equal(contractPayload.contract_id, '018f4b2c-1111-7222-8333-444455556666');
  assert.equal(
    Object.prototype.hasOwnProperty.call(contractPayload, 'supplier_id'),
    false,
    '合同采购的供应商由服务端从合同乙方派生，前端不该发（发了不一致就 400 66025）',
  );

  const spotForm = emptyPurchaseForm();
  applyPurchaseType(spotForm, PURCHASE_TYPE_SPOT);
  spotForm.supplier_id = '018f4b2c-2222-7222-8333-444455556666';
  const spotPayload = buildPurchasePayload(spotForm);
  assert.equal(spotPayload.purchase_type, 'SPOT');
  assert.equal(spotPayload.supplier_id, '018f4b2c-2222-7222-8333-444455556666');
  assert.equal(spotPayload.contract_id, null, 'SPOT 带 contract_id 会被 DTO 拦（400）');
});

// ---------------------------------------------------------------- ③ 入库单

test('⑪ 入库单采购入库：供应商只读（值来自采购单）+ 采购单必选', () => {
  const source = frontendSource(STOCKIN_VIEW);
  const form = emptyStockInForm();

  assert.equal(form.sourceless, false, '新建默认采购入库');
  assert.equal(isSupplierReadonly(form), true);
  assert.equal(supplierDisplayText(form), '请先选择采购单');
  assert.equal(supplierHintText(form), '供应商来自采购单，不可修改');
  // 没选采购单 → 校验拦
  assert.ok(validateStockInForm(form).purchase_order_id);

  // 选了采购单：供应商换成采购单上的名字，且前端不再传 supplier_id
  form.purchase_order_id = '018f4b2c-3333-7222-8333-444455556666';
  form.supplier_id = '018f4b2c-2222-7222-8333-444455556666';
  form.supplier_name = '北京钢材有限公司';
  form.warehouse_id = '018f4b2c-4444-7222-8333-444455556666';
  form.items = [
    {
      key: 'row-1',
      purchase_order_item_id: '018f4b2c-7777-7222-8333-444455556666',
      material_id: '018f4b2c-8888-7222-8333-444455556666',
      qty: 1,
      price_excl: 10,
      tax_rate: 13,
    },
  ];
  assert.equal(supplierDisplayText(form), '北京钢材有限公司');
  assert.equal(validateStockInForm(form).purchase_order_id, undefined);

  const payload = buildStockInPayload(form);
  assert.equal(payload.purchase_order_id, '018f4b2c-3333-7222-8333-444455556666');
  assert.equal(
    Object.prototype.hasOwnProperty.call(payload, 'supplier_id'),
    false,
    'A4：有源入库单不传 supplier_id（服务端派生，传了不一致 → 400）',
  );
  assert.equal(Object.prototype.hasOwnProperty.call(payload, 'is_sourceless'), false);

  // 模板层：只读输入框 + 说明行
  assert.match(source, /v-if="isSupplierReadonly\(form\)"[\s\S]{0,200}data-testid="stockin-supplier-readonly"/);
  assert.match(source, /data-testid="stockin-supplier-hint"/);
});

test('⑫ 入库单非采购入库（无源单据）：显示「无源单据」标记，供应商转手选且必须写备注', () => {
  const source = frontendSource(STOCKIN_VIEW);
  const form = emptyStockInForm();
  applySourceMode(form, true);

  assert.equal(form.sourceless, true);
  assert.equal(showsSourcelessMark(form), true);
  assert.equal(isSupplierReadonly(form), false, '无源单没有采购单可带出，后端要求自己指定供应商');
  assert.match(supplierHintText(form), /无源单据/);
  // 模板层：标记 tag + 手选供应商 + 模式单选
  assert.match(source, /data-testid="stockin-sourceless-mark"/);
  assert.match(source, /v-else[\s\S]{0,160}data-testid="stockin-supplier"/);

  // 无源单：缺供应商 / 缺备注都拦（后端 400 66027 / 66029）
  const errors = validateStockInForm(form);
  assert.ok(errors.supplier_id);
  assert.ok(errors.remark);

  form.supplier_id = '018f4b2c-2222-7222-8333-444455556666';
  form.warehouse_id = '018f4b2c-4444-7222-8333-444455556666';
  form.remark = '期初余额导入（2026-10 期末盘点结转）';
  form.items = [
    {
      key: 'row-1',
      material_id: '018f4b2c-8888-7222-8333-444455556666',
      qty: 2,
      price_excl: 5,
      tax_rate: 13,
    },
  ];
  assert.deepEqual(validateStockInForm(form), {});

  const payload = buildStockInPayload(form);
  assert.equal(payload.is_sourceless, true, '无源必须显式声明（AP-5：不能靠空值隐式表达）');
  assert.equal(payload.supplier_id, '018f4b2c-2222-7222-8333-444455556666');
  assert.equal(payload.purchase_order_id, null);
});

test('⑬ 入库单未选采购单 → 供应商显示「请先选择采购单」（不是空白、不是 —）', () => {
  const form = emptyStockInForm();
  assert.equal(supplierDisplayText(form), '请先选择采购单');
  assert.equal(form.supplier_name, '');
  // 有 id 但名称缺失时回落到主档查找，不显示裸 id
  form.purchase_order_id = 'po-1';
  form.supplier_id = 'sup-1';
  assert.equal(supplierDisplayText(form, () => '主档供应商名'), '主档供应商名');
  // 无源模式下不显示那句话（无源本来就没有采购单）
  const sourceless = emptyStockInForm();
  applySourceMode(sourceless, true);
  assert.equal(supplierDisplayText(sourceless), '');
});

test('入库单：采购单下拉只列 APPROVED / PARTIAL_RECEIVED，且详情回填保留无源标记', () => {
  const rows = [
    { id: 'a', status: 'APPROVED' },
    { id: 'b', status: 'PARTIAL_RECEIVED' },
    { id: 'c', status: 'DRAFT' },
    { id: 'd', status: 'SUBMITTED' },
    { id: 'e', status: 'RECEIVED' },
    { id: 'f', status: 'CANCELLED' },
  ];
  assert.deepEqual(
    selectablePurchaseOrders(rows).map((row) => row.id),
    ['a', 'b'],
  );
  assert.deepEqual([...RECEIVABLE_ORDER_STATUSES], ['APPROVED', 'PARTIAL_RECEIVED']);

  // 下拉文案：单号 + 供应商 + 状态（选单时要能一眼确认）
  assert.equal(
    purchaseOrderOptionLabel({ order_no: 'PO-1', supplier_name: '北京钢材有限公司', status_label: '已审批' }),
    'PO-1 · 北京钢材有限公司（已审批）',
  );

  // 详情回填：无源单不能被洗成有源（A4 的 is_sourceless 是独立列）
  const sourcelessForm = stockInDetailToForm(
    stockInRow({ is_sourceless: true, supplier_id: 'sup-1', supplier_name: '北京钢材有限公司' }),
    [],
  );
  assert.equal(sourcelessForm.sourceless, true);
  assert.equal(sourcelessForm.supplier_name, '北京钢材有限公司');
  const sourcedForm = stockInDetailToForm(
    stockInRow({ source_order_id: 'po-1', source_order_no: 'PO-1', supplier_id: 'sup-1' }),
    [],
  );
  assert.equal(sourcedForm.sourceless, false);
  assert.equal(sourcedForm.purchase_order_no, 'PO-1');
});
