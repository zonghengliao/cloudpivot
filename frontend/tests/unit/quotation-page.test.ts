/**
 * 报价单页面（M3-06）纯逻辑单测 + 「源码即契约」对拍。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * # 为什么测 `.ts` 不测 `.vue`
 *
 * 「哪些边要原因」「能不能转单」「合计怎么算」「请求发了什么」全在
 * `quotation-logic.ts` 里，`.vue` 只渲染。这些一旦漂移 `vue-tsc` 抓不到
 * （两边都是 `string` / `number`），线上表现是**算错钱**或**按钮点出 409**。
 *
 * # 断言口径照后端抄（照卡面抄会把 bug 钉成预期）
 *
 * - `crates/cloudpivot-kernel/.../state_machine.rs`：`QUOTATION_EDGES` / `QUOTATION_REASON_EDGES`
 * - `app/src/business/erp/sales/quotation_model.rs`：`STATUSES` / `status_label` / `MAX_LINES`
 * - `app/migrations/business/V0433__quotation_permissions.up.sql`：页面节点 uuid + 权限码
 * - `app/migrations/business/V0436__quotation_page_menu.up.sql`：本卡补的 path / component
 *
 * 其中「边表 / 原因边 / 权限码 / 菜单 path」直接 `readFileSync` 读后端源码断言逐字包含，
 * 后端改字面量、前端抄错都会当场红。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';

import type { Quotation } from '@/api/business/erp/quotation';

import {
  CONVERT_SOURCE_HINT,
  EDGE_LABELS,
  MAX_LINES,
  MAX_LINE_REMARK,
  PAGE_SIZE,
  QUOTATION_EDGES,
  QUOTATION_REASON_EDGES,
  addDays,
  approvalBadgeOf,
  buildConvertPayload,
  buildCreatePayload,
  buildQuery,
  buildUpdatePayload,
  canSubmitApproval,
  convertResultText,
  convertStateOf,
  convertedRefText,
  emptyConvertForm,
  emptyFilter,
  emptyForm,
  emptyListText,
  formFrom,
  isFiltered,
  lineAmountsOf,
  needsReason,
  newLine,
  round4,
  statusActionsOf,
  toNumber,
  todayCst,
  totalsOf,
  validateConvertForm,
  validateForm,
  type FormDraft,
} from '@/views/business/erp/quotation/quotation-logic';

const HERE = dirname(fileURLToPath(import.meta.url));
/** 仓库根（tests/unit → frontend → 仓库根） */
const REPO_ROOT = resolve(HERE, '..', '..', '..');

function readBackend(relative: string): string {
  return readFileSync(resolve(REPO_ROOT, relative), 'utf8');
}

/** 造一张报价单（只填断言要用的字段，其余按最小合法值） */
function quotation(overrides: Partial<Quotation> = {}): Quotation {
  return {
    id: 'c0a10000-0000-4000-8000-00000000e001',
    quote_no: 'QT-20261117-000001',
    customer_id: 'c0a10000-0000-4000-8000-00000000c001',
    customer_code: 'C001',
    customer_name: '北京示例客户',
    quote_date: '2026-11-17',
    valid_until: '2026-12-01',
    expired: false,
    status: 'DRAFT',
    status_label: '草稿',
    total_amount: '0.0000',
    total_tax: '0.0000',
    total_with_tax: '0.0000',
    converted_so_id: null,
    converted_at: null,
    approval_instance_id: null,
    approval: null,
    remark: null,
    editable: true,
    deletable: true,
    allowed_targets: ['SENT', 'CANCELLED'],
    items: [],
    created_at: '2026-11-17T02:00:00Z',
    updated_at: '2026-11-17T02:00:00Z',
    create_by: null,
    update_by: null,
    ...overrides,
  };
}

// ==================== 源码即契约 ====================

test('边表与后端 QUOTATION_EDGES 逐条一致', () => {
  const source = readBackend('backend/crates/cloudpivot-kernel/src/business/shared/state_machine.rs');
  for (const [from, to] of QUOTATION_EDGES) {
    assert.ok(
      source.includes(`("${from}", "${to}")`),
      `后端 edge 表里找不到 (${from}, ${to}) —— 前端边表漂移了`,
    );
  }
  // 条数也要对（6 条；多一条少一条都说明抄错）
  const block = source.slice(
    source.indexOf('pub const QUOTATION_EDGES'),
    source.indexOf('pub const QUOTATION_BASE'),
  );
  const edges = [...block.matchAll(/\("([A-Z]+)", "([A-Z]+)"\)/g)].map((m) => `${m[1]}->${m[2]}`);
  assert.equal(edges.length, 6, `后端 QUOTATION_EDGES 应有 6 条，解析到 ${edges.length} 条`);
  assert.deepEqual(
    QUOTATION_EDGES.map(([f, t]) => `${f}->${t}`),
    edges,
    '前端边表顺序 / 内容与后端不一致',
  );
});

test('需要原因的边与后端 QUOTATION_REASON_EDGES 逐条一致（恰好 3 条）', () => {
  const source = readBackend('backend/crates/cloudpivot-kernel/src/business/shared/state_machine.rs');
  const block = source.slice(
    source.indexOf('pub const QUOTATION_REASON_EDGES'),
    source.indexOf('报价单入账态'),
  );
  const edges = [...block.matchAll(/\("([A-Z]+)", "([A-Z]+)"\)/g)].map((m) => `${m[1]}->${m[2]}`);
  assert.equal(edges.length, 3, `后端原因边应 3 条，解析到 ${edges.length}`);
  assert.deepEqual(
    QUOTATION_REASON_EDGES.map(([f, t]) => `${f}->${t}`),
    edges,
    '前端原因边与后端不一致',
  );
});

test('MAX_LINES / 状态标签与后端源码一致', () => {
  const handler = readBackend('backend/app/src/business/erp/sales/quotation_handler.rs');
  assert.ok(handler.includes(`pub const MAX_LINES: usize = ${MAX_LINES};`), 'MAX_LINES 与后端不一致');

  const model = readBackend('backend/app/src/business/erp/sales/quotation_model.rs');
  const labels: Array<[string, string]> = [
    ['STATUS_DRAFT', '草稿'],
    ['STATUS_SENT', '已发出'],
    ['STATUS_ACCEPTED', '已接受'],
    ['STATUS_REJECTED', '已拒绝'],
    ['STATUS_CANCELLED', '已作废'],
  ];
  for (const [code, label] of labels) {
    assert.match(
      model,
      new RegExp(`${code}\\s*=>\\s*"${label}"`),
      `后端 status_label 里找不到 ${code} => ${label}`,
    );
  }
});

test('5 个权限码在 V0433 迁移里逐字存在（前端 PERM 常量不能改一个字）', () => {
  const migration = readBackend('backend/app/migrations/business/V0433__quotation_permissions.up.sql');
  const codes = [
    'business:erp:quotation:list',
    'business:erp:quotation:create',
    'business:erp:quotation:update',
    'business:erp:quotation:delete',
    'business:erp:quotation:status',
  ];
  for (const code of codes) {
    assert.ok(migration.includes(code), `V0433 里找不到 ${code}`);
  }
  assert.ok(
    migration.includes('20000000-0000-0000-0000-000000004331'),
    'V0433 的页面节点 uuid 变了，V0436 / 前端注释都要跟着改',
  );
});

test('本卡迁移 V0436 补的 path / component 与前端路由逐字一致', () => {
  const migration = readBackend('backend/app/migrations/business/V0436__quotation_page_menu.up.sql');
  assert.ok(migration.includes("'/business/erp/quotation'"), 'V0436 未补 path');
  assert.ok(migration.includes("'business/erp/quotation/index'"), 'V0436 未补 component');

  const router = readBackend('frontend/src/router/index.ts');
  assert.ok(
    router.includes("path: 'business/erp/quotation'"),
    'router/index.ts 未注册 business/erp/quotation（n08 会红）',
  );
  assert.ok(
    router.includes("@/views/business/erp/quotation/index.vue"),
    'router 指向的组件路径不对',
  );
  assert.ok(
    router.includes("permission: 'business:erp:quotation:list'"),
    '路由 meta.permission 必须是 :list 码（页面显形由侧栏菜单管）',
  );
});

// ==================== 金额 ====================

test('行金额：金额 = round4(单价 × 数量)，税额 = round4(金额 × 税率/100)', () => {
  // 12.3456 × 3 = 37.0368（4 位不丢精度）
  assert.deepEqual(lineAmountsOf({ qty: 3, price: '12.3456', tax_rate: 0 }), {
    amount: 37.0368,
    taxAmount: 0,
    withTax: 37.0368,
  });
  // 税额 13%：37.0368 × 0.13 = 4.814784 → 4.8148
  assert.deepEqual(lineAmountsOf({ qty: 3, price: '12.3456', tax_rate: 13 }), {
    amount: 37.0368,
    taxAmount: 4.8148,
    withTax: 41.8516,
  });
  // 0.1 × 3 的浮点陷阱：必须是 0.3 而不是 0.30000000000000004
  assert.equal(lineAmountsOf({ qty: 3, price: 0.1, tax_rate: 0 }).amount, 0.3);
});

test('合计：逐行先 round4 再求和，价税合计 = 不含税 + 税额', () => {
  const totals = totalsOf([
    { qty: 3, price: '12.3456', tax_rate: 13 },
    { qty: 1, price: '100', tax_rate: 13 },
  ]);
  assert.equal(totals.amount, round4(37.0368 + 100));
  assert.equal(totals.tax, round4(4.8148 + 13));
  assert.equal(totals.withTax, round4(totals.amount + totals.tax));
});

test('数值化：空串 / null / 非法一律 0（不是 NaN，NaN 会静默写进合计）', () => {
  assert.equal(toNumber(''), 0);
  assert.equal(toNumber(null), 0);
  assert.equal(toNumber(undefined), 0);
  assert.equal(toNumber('abc'), 0);
  assert.equal(toNumber('12.500000'), 12.5);
});

// ==================== 筛选 ====================

test('筛选拼参：空值整键缺席、expired 三态、page_size 恒带', () => {
  const empty = buildQuery(emptyFilter(), 1);
  assert.equal(empty.quote_no, undefined);
  assert.equal(empty.status, undefined);
  assert.equal(empty.customer_id, undefined);
  assert.equal(empty.expired, undefined, 'ALL 必须不传 expired（三态口径：不传 = 全部）');
  assert.equal(empty.page, 1);
  assert.equal(empty.page_size, PAGE_SIZE);
});

test('筛选拼参：有值时逐键带上，quote_no 去空格', () => {
  const query = buildQuery(
    { quote_no: '  QT-2026  ', status: 'ACCEPTED', customer_id: 'c-1', expired: 'EXPIRED' },
    3,
    50,
  );
  assert.equal(query.quote_no, 'QT-2026');
  assert.equal(query.status, 'ACCEPTED');
  assert.equal(query.customer_id, 'c-1');
  assert.equal(query.expired, true);
  assert.equal(query.page, 3);
  assert.equal(query.page_size, 50);

  assert.equal(buildQuery({ ...emptyFilter(), expired: 'ACTIVE' }, 1).expired, false);
  assert.equal(buildQuery({ ...emptyFilter(), expired: 'EXPIRED' }, 1).expired, true);
});

test('isFiltered / 空态文案：三种空筛选不算「筛没了」', () => {
  assert.equal(isFiltered(emptyFilter()), false);
  assert.equal(isFiltered({ ...emptyFilter(), status: 'SENT' }), true);
  assert.equal(isFiltered({ ...emptyFilter(), quote_no: '   ' }), false, '纯空格不算筛选');
  assert.notEqual(emptyListText(true), emptyListText(false));
});

// ==================== 表单 ====================

test('空表单：1 行明细 + 有效期默认 14 天（后端不默认，页面预填是为少一次 400）', () => {
  const form = emptyForm('2026-11-17');
  assert.equal(form.quote_date, '2026-11-17');
  assert.equal(form.valid_until, '2026-12-01');
  assert.equal(form.items.length, 1);
  assert.equal(form.customer_id, '');
});

test('addDays 跨月 / 跨年', () => {
  assert.equal(addDays('2026-11-30', 1), '2026-12-01');
  assert.equal(addDays('2026-12-31', 1), '2027-01-01');
  assert.equal(addDays('2024-02-28', 1), '2024-02-29', '闰年 2 月');
});

test('todayCst 是北京时间（UTC+8）而不是 UTC', () => {
  const cst = todayCst();
  const utc = new Date().toISOString().slice(0, 10);
  // 北京时间 >= UTC 日期（最多差一天：UTC 16:00 之后北京已经是次日）
  const delta = (Date.parse(`${cst}T00:00:00Z`) - Date.parse(`${utc}T00:00:00Z`)) / 86400000;
  assert.ok(delta === 0 || delta === 1, `todayCst=${cst} 与 UTC 日期 ${utc} 差 ${delta} 天`);
});

test('校验顺序：客户 → 报价日期 → 有效期 → 明细（先给人话，不先提数量）', () => {
  const base = emptyForm('2026-11-17');
  assert.match(validateForm(base), /请选择客户/);

  const noDate = { ...base, customer_id: 'c-1', quote_date: '' };
  assert.match(validateForm(noDate), /报价日期/);

  const noValid = { ...base, customer_id: 'c-1', valid_until: '' };
  assert.match(validateForm(noValid), /有效期/);

  const reversed = { ...base, customer_id: 'c-1', valid_until: '2026-11-16' };
  assert.match(validateForm(reversed), /不能早于报价日期/);

  const emptyItems: FormDraft = { ...base, customer_id: 'c-1', items: [] };
  assert.match(validateForm(emptyItems), /至少要 1 行明细/);
});

test('校验：明细逐条（物料 / 数量 / 单价 / 税率 / 备注）', () => {
  const base: FormDraft = {
    customer_id: 'c-1',
    quote_date: '2026-11-17',
    valid_until: '2026-12-01',
    remark: '',
    items: [{ ...newLine(), material_id: 'm-1', qty: 2, price: 10, tax_rate: 0 }],
  };
  assert.equal(validateForm(base), '', '最小合法单应通过');

  assert.match(
    validateForm({ ...base, items: [{ ...base.items[0], material_id: '' }] }),
    /第 1 行还没选物料/,
  );
  assert.match(validateForm({ ...base, items: [{ ...base.items[0], qty: 0 }] }), /数量必须大于 0/);
  assert.match(validateForm({ ...base, items: [{ ...base.items[0], qty: '' }] }), /数量必须大于 0/);
  assert.match(validateForm({ ...base, items: [{ ...base.items[0], price: -1 }] }), /单价不能为负/);
  assert.match(
    validateForm({ ...base, items: [{ ...base.items[0], tax_rate: 101 }] }),
    /税率必须在 0–100 之间/,
  );
  assert.match(
    validateForm({
      ...base,
      items: [{ ...base.items[0], remark: 'x'.repeat(MAX_LINE_REMARK + 1) }],
    }),
    /行备注超过/,
  );
});

test('校验：全 0 金额静默单要拦（发给客户没有任何意义）', () => {
  const form: FormDraft = {
    customer_id: 'c-1',
    quote_date: '2026-11-17',
    valid_until: '2026-12-01',
    remark: '',
    items: [{ ...newLine(), material_id: 'm-1', qty: 1, price: 0, tax_rate: 0 }],
  };
  assert.match(validateForm(form), /金额都是 0/);
});

test('校验：超过 200 行明细（后端 MAX_LINES）', () => {
  const items = Array.from({ length: MAX_LINES + 1 }, () => ({
    ...newLine(),
    material_id: 'm-1',
    qty: 1,
    price: 1,
    tax_rate: 0,
  }));
  const form: FormDraft = {
    customer_id: 'c-1',
    quote_date: '2026-11-17',
    valid_until: '2026-12-01',
    remark: '',
    items,
  };
  assert.match(validateForm(form), new RegExp(`最多 ${MAX_LINES} 行`));
});

test('建单请求体：金额不传、空备注转 null、qty/price 数值化', () => {
  const form: FormDraft = {
    customer_id: 'c-1',
    quote_date: '2026-11-17',
    valid_until: '2026-12-01',
    remark: '  付款条件 30 天  ',
    items: [{ ...newLine(), material_id: 'm-1', qty: '2', price: '12.5', tax_rate: '13', remark: '  ' }],
  };
  const payload = buildCreatePayload(form);
  assert.equal(payload.customer_id, 'c-1');
  assert.equal(payload.valid_until, '2026-12-01');
  assert.equal(payload.remark, '付款条件 30 天', '头备注要去空格');
  assert.deepEqual(payload.items, [
    { material_id: 'm-1', qty: 2, price: 12.5, tax_rate: 13, remark: null },
  ]);
  assert.equal('amount' in payload.items[0], false, '金额由后端现算，前端不许传');
});

test('改单请求体 = 建单请求体（明细整单替换，不能只发改动行）', () => {
  const form = emptyForm('2026-11-17');
  form.customer_id = 'c-1';
  form.items = [{ ...newLine(), material_id: 'm-1', qty: 1, price: 5, tax_rate: 0 }];
  assert.deepEqual(buildUpdatePayload(form), buildCreatePayload(form));
});

test('详情 → 表单：日期 / 金额 / 备注原样回填，行 key 重新生成，空明细给 1 行', () => {
  const body = quotation({
    status: 'DRAFT',
    remark: '含运费',
    items: [
      {
        id: 'l-1',
        line_no: 1,
        material_id: 'm-1',
        material_code: 'M001',
        material_name: '物料一',
        spec: null,
        unit_name: '个',
        qty: '2.0000',
        price: '12.500000',
        amount: '25.0000',
        tax_rate: '13.0000',
        tax_amount: '3.2500',
        with_tax: '28.2500',
        remark: '交期 7 天',
      },
    ],
  });
  const form = formFrom(body);
  assert.equal(form.customer_id, body.customer_id);
  assert.equal(form.quote_date, '2026-11-17');
  assert.equal(form.valid_until, '2026-12-01');
  assert.equal(form.remark, '含运费');
  assert.equal(form.items.length, 1);
  assert.equal(form.items[0].qty, 2);
  assert.equal(form.items[0].price, 12.5);
  assert.equal(form.items[0].tax_rate, 13);
  assert.equal(form.items[0].remark, '交期 7 天');
  assert.ok(form.items[0].key.startsWith('qt-line-'));

  const empty = formFrom(quotation({ items: [] }));
  assert.equal(empty.items.length, 1, '空明细也要给一行，否则表单里没有输入框');
});

// ==================== 状态 / 转单 ====================

test('可执行动作：DRAFT 只有「作废草稿」（发出走审批，不在改状态里）', () => {
  const actions = statusActionsOf('DRAFT', ['SENT', 'CANCELLED']);
  assert.deepEqual(
    actions.map((a) => a.target),
    ['CANCELLED'],
    'DRAFT→SENT 必须从「改状态」按钮里剔除，否则点了必然 409+66088',
  );
  assert.equal(actions[0].label, EDGE_LABELS['DRAFT->CANCELLED']);
  assert.equal(actions[0].needsReason, false, '草稿作废不需要原因');
});

test('可执行动作：SENT 有三条（接受 / 拒绝 / 作废），且只有作废在三条边里重复出现时不会串', () => {
  const actions = statusActionsOf('SENT', ['ACCEPTED', 'REJECTED', 'CANCELLED']);
  assert.deepEqual(actions.map((a) => a.target), ['ACCEPTED', 'REJECTED', 'CANCELLED']);
  assert.equal(actions.find((a) => a.target === 'ACCEPTED')?.needsReason, false);
  assert.equal(actions.find((a) => a.target === 'REJECTED')?.needsReason, true);
  assert.equal(actions.find((a) => a.target === 'CANCELLED')?.label, EDGE_LABELS['SENT->CANCELLED']);
});

test('可执行动作：ACCEPTED 只有「作废已接受的报价」，标签不能串成「作废草稿」', () => {
  const actions = statusActionsOf('ACCEPTED', ['CANCELLED']);
  assert.equal(actions.length, 1, '只按目标态过滤会把三条作废边全列出来（曾经的真 bug）');
  assert.equal(actions[0].label, EDGE_LABELS['ACCEPTED->CANCELLED']);
  assert.equal(actions[0].needsReason, true);
});

test('可执行动作：终态（REJECTED / CANCELLED）没有按钮', () => {
  assert.deepEqual(statusActionsOf('REJECTED', []), []);
  assert.deepEqual(statusActionsOf('CANCELLED', []), []);
});

test('needsReason 只认那 3 条边', () => {
  assert.equal(needsReason('DRAFT', 'SENT'), false);
  assert.equal(needsReason('DRAFT', 'CANCELLED'), false);
  assert.equal(needsReason('SENT', 'ACCEPTED'), false);
  assert.equal(needsReason('SENT', 'REJECTED'), true);
  assert.equal(needsReason('SENT', 'CANCELLED'), true);
  assert.equal(needsReason('ACCEPTED', 'CANCELLED'), true);
  assert.equal(needsReason('REJECTED', 'DRAFT'), false, '不存在的边也不该要原因');
});

test('转单按钮：只有 ACCEPTED 且未转过才可点', () => {
  assert.equal(convertStateOf(quotation({ status: 'ACCEPTED' })).enabled, true);
  assert.equal(convertStateOf(quotation({ status: 'DRAFT' })).enabled, false);
  assert.equal(convertStateOf(quotation({ status: 'SENT' })).enabled, false);
  assert.equal(convertStateOf(quotation({ status: 'REJECTED' })).enabled, false);
  assert.equal(convertStateOf(quotation({ status: 'CANCELLED' })).enabled, false);
  assert.equal(
    convertStateOf(quotation({ status: 'ACCEPTED', converted_so_id: 'so-1' })).enabled,
    false,
    '已转过的单不能再给第二次机会（后端幂等也只是回原单）',
  );
  // 已接受但今天过期仍可转（过期约束的是「接受时」）
  assert.equal(convertStateOf(quotation({ status: 'ACCEPTED', expired: true })).enabled, true);
  // 每个 disabled 都必须说清为什么
  for (const status of ['DRAFT', 'SENT', 'REJECTED', 'CANCELLED'] as const) {
    assert.ok(convertStateOf(quotation({ status })).hint.length > 0, `${status} 的置灰原因不能是空话`);
  }
});

test('转单请求体：空值整键缺席（不发 null，少一个歧义）', () => {
  assert.deepEqual(buildConvertPayload(emptyConvertForm('2026-11-17')), {
    order_date: '2026-11-17',
    delivery_date: undefined,
    salesperson_id: undefined,
    remark: undefined,
  });
  assert.deepEqual(
    buildConvertPayload({
      order_date: '2026-11-17',
      delivery_date: '2026-12-01',
      salesperson_id: 'u-1',
      remark: '  加急  ',
    }),
    { order_date: '2026-11-17', delivery_date: '2026-12-01', salesperson_id: 'u-1', remark: '加急' },
  );
});

test('转单表单校验：交期不能早于下单日', () => {
  assert.equal(validateConvertForm(emptyConvertForm('2026-11-17')), '');
  assert.match(
    validateConvertForm({
      order_date: '2026-11-17',
      delivery_date: '2026-11-16',
      salesperson_id: '',
      remark: '',
    }),
    /不能早于下单日期/,
  );
});

test('转单结果文案：幂等命中必须显式说「没建第二张」', () => {
  const header = {
    so_id: 'so-1',
    so_no: 'SO-20261117-000001',
    status: 'DRAFT',
    status_label: '草稿',
    customer_id: 'c-1',
    customer_code: 'C001',
    customer_name: '客户',
    order_date: '2026-11-17',
    delivery_date: null,
    salesperson_id: null,
    salesperson_name: null,
    total_amount: '10',
    total_tax: '1.3',
    total_with_tax: '11.3',
    currency_code: 'CNY',
    exchange_rate: '1',
    base_total_amount: '10',
    base_total_tax: '1.3',
    base_total_with_tax: '11.3',
    remark: '由报价单 QT-20261117-000001 转换',
  };
  const fresh = convertResultText({ header, items: [], meta: { already_converted: false, line_count: 3 } });
  assert.match(fresh, /已生成销售订单 SO-20261117-000001/);
  assert.match(fresh, /3 行/);

  const again = convertResultText({ header, items: [], meta: { already_converted: true, line_count: 3 } });
  assert.match(again, /之前已转过/);
  assert.match(again, /未新建第二张/);
});

test('转单回链文案：未转单是破折号，转过带订单号 + 时间', () => {
  assert.equal(convertedRefText(quotation()), '—');
  const text = convertedRefText(
    quotation({ converted_so_id: 'c0a10000-0000-4000-8000-00000000so01', converted_at: '2026-11-17T03:04:05Z' }),
    'SO-20261117-000001',
  );
  assert.match(text, /SO-20261117-000001/);
  assert.match(text, /2026-11-17 03:04/);
  // 查不到订单号时退回短 id（不能显示成空）
  const fallback = convertedRefText(quotation({ converted_so_id: 'c0a10000-0000-4000-8000-00000000so01' }));
  assert.match(fallback, /c0a10000/);
});

// ==================== 审批 ====================

test('审批徽标：PENDING 带待办人数，非 PENDING 不显示人数', () => {
  const pending = approvalBadgeOf(
    quotation({
      approval: {
        instance_id: 'i-1',
        instance_no: 'FLW-20261117-00000001',
        status: 'PENDING',
        status_label: '审批中',
        pending_tasks: 2,
      },
    }),
  );
  assert.equal(pending?.theme, 'warning');
  assert.match(pending?.text ?? '', /还等 2 人/);

  const approved = approvalBadgeOf(
    quotation({
      approval: {
        instance_id: 'i-1',
        instance_no: 'FLW-20261117-00000001',
        status: 'APPROVED',
        status_label: '已通过',
        pending_tasks: 0,
      },
    }),
  );
  assert.equal(approved?.theme, 'success');
  assert.doesNotMatch(approved?.text ?? '', /还等/);

  assert.equal(approvalBadgeOf(quotation()), null, '从没提交过审批 → 不显示徽标');
});

test('提交审批按钮：只有 DRAFT + 没有在跑/已通过实例才可点', () => {
  const draft = quotation({
    status: 'DRAFT',
    items: [
      {
        id: 'l-1',
        line_no: 1,
        material_id: 'm-1',
        material_code: null,
        material_name: null,
        spec: null,
        unit_name: null,
        qty: '1',
        price: '1',
        amount: '1',
        tax_rate: '0',
        tax_amount: '0',
        with_tax: '1',
        remark: null,
      },
    ],
  });
  assert.equal(canSubmitApproval(draft).enabled, true);

  const sent = quotation({ status: 'SENT' });
  assert.equal(canSubmitApproval(sent).enabled, false);
  assert.match(canSubmitApproval(sent).hint, /只有草稿/);

  const pending = quotation({
    status: 'DRAFT',
    approval: {
      instance_id: 'i-1',
      instance_no: 'FLW-1',
      status: 'PENDING',
      status_label: '审批中',
      pending_tasks: 1,
    },
  });
  assert.equal(canSubmitApproval(pending).enabled, false);
  assert.match(canSubmitApproval(pending).hint, /已在审批中/);

  // 退回发起人（PENDING 但待办在发起人手里）→ 不许再走 submit-approval（后端 409，要「重新提交」）
  const resubmit = quotation({
    status: 'DRAFT',
    approval: {
      instance_id: 'i-1',
      instance_no: 'FLW-1',
      status: 'PENDING',
      status_label: '审批中',
      pending_tasks: 0,
    },
  });
  assert.equal(canSubmitApproval(resubmit).enabled, false);
  assert.match(canSubmitApproval(resubmit).hint, /重新提交/);

  const approved = quotation({
    status: 'DRAFT',
    approval: {
      instance_id: 'i-1',
      instance_no: 'FLW-1',
      status: 'APPROVED',
      status_label: '已通过',
      pending_tasks: 0,
    },
  });
  assert.equal(canSubmitApproval(approved).enabled, false);
  assert.match(canSubmitApproval(approved).hint, /直接「发出」/);
});

test('转单提示文案里必须点名来源不会被覆盖', () => {
  assert.match(CONVERT_SOURCE_HINT, /由报价单/);
});
