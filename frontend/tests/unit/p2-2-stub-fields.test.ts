/**
 * P2-2 摆设字段处置的**前端口径**单测（ARCH-3 §5 DC-12）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖、无 DOM）。
 *
 * # 为什么要有这一组
 *
 * 后端把口径做实了（信用额度现算 + 强制审批 / 安全库存 0 不预警 / 锁定量预留），
 * 前端如果还按老样子显示，用户看到的仍然是「填了不知道有没有生效」：
 *
 * | 字段 | 后端真值 | 前端必须显示成 |
 * |---|---|---|
 * | `credit_exposure = null` | **没查过**（草稿） | 「未评估」，**不能**显示「额度内」 |
 * | `over_credit_limit = true` | 超限，已强制走审批 | 「超信用额度」红色标签 + 累计/额度 tooltip |
 * | `credit_limit = 0` | 不限额度（V0137 语义） | 「额度不限」，**不能**显示「额度 0 / 已超 0」 |
 * | `safety_stock` 留空 | 落库 `0` = 不预警 | 输入框提示「留空 = 不提醒」 |
 * | `locked_qty` | 预留列，恒 0 | **不显示这一列**（显示 0 会让人以为占用机制在工作） |
 *
 * 前四条是纯函数（{@link creditVerdictOf} / {@link safetyStockOf}），最后一条只能靠
 * 「源码即契约」断言（列定义是 `t-table` 的 `columns` 常量，node 里没有 DOM 可渲染）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

import type { PurchaseOrderRow } from '@/api/business/erp';
import { creditVerdictOf } from '@/views/business/erp/purchase/purchase-logic';
import {
  SAFETY_STOCK_HINT,
  buildMaterialCreatePayload,
  buildMaterialUpdatePayload,
  emptyMaterialForm,
  materialFormFromRow,
  quantityText,
  safetyStockOf,
  validateMaterialForm,
} from '@/views/business/erp/material/material-query';

const ROOT = resolve(process.cwd());

/** 读一页 SFC / 模块源码（断言只做子串与正则，不做 AST —— 与既有源码契约测同风格） */
function sourceOf(relative: string): string {
  return readFileSync(resolve(ROOT, relative), 'utf8');
}

/** 一张「提交过、未超限」的采购单（只填本组断言用到的字段）。 */
function order(over: Partial<PurchaseOrderRow> = {}): PurchaseOrderRow {
  return {
    id: 'po-1',
    order_no: 'PO-1',
    supplier_id: 'sup-1',
    order_date: '2026-11-04',
    expect_date: null,
    warehouse_id: 'wh-1',
    status: 'SUBMITTED',
    currency_code: 'CNY',
    total_qty: '1',
    total_amount: '100',
    total_tax: '0',
    total_with_tax: '100',
    credit_days: 0,
    remark: null,
    price_mode: 'EXCL',
    purchase_type: 'SPOT',
    contract_id: null,
    contract_no: null,
    contract_no_snapshot: null,
    supplier_name: null,
    supplier_name_snapshot: null,
    submitted_at: null,
    submitted_by: null,
    approved_at: null,
    approved_by: null,
    closed_at: null,
    closed_by: null,
    approval_instance_id: null,
    approval_instance_no: null,
    over_credit_limit: false,
    credit_exposure: null,
    credit_limit_snapshot: null,
    created_at: '2026-11-04T00:00:00Z',
    updated_at: '2026-11-04T00:00:00Z',
    ...over,
  };
}

// ==================== ① 信用额度三态 ====================

test('① 未评估（credit_exposure 为 null）= 显示「未评估」，不能显示成「额度内」', () => {
  const verdict = creditVerdictOf(order());
  assert.equal(verdict.known, false);
  assert.equal(verdict.over, false);
  assert.equal(verdict.label, '未评估');
  assert.match(verdict.tip, /提交时才现算/);
});

test('① 超限：红色标签 + tooltip 带累计与额度（后端 Decimal 出参是字符串，要格式化）', () => {
  const verdict = creditVerdictOf(
    order({ over_credit_limit: true, credit_exposure: '20000.0000', credit_limit_snapshot: '10000.0000' }),
  );
  assert.equal(verdict.known, true);
  assert.equal(verdict.over, true);
  assert.equal(verdict.label, '超信用额度');
  assert.match(verdict.tip, /20,000/);
  assert.match(verdict.tip, /10,000/);
  assert.match(verdict.tip, /必须走审批/);
});

test('① 额度内：tooltip 给「累计 / 额度 / 剩余」三个数，且剩余 = 额度 − 累计', () => {
  const verdict = creditVerdictOf(
    order({ credit_exposure: '8000.0000', credit_limit_snapshot: '100000.0000' }),
  );
  assert.equal(verdict.label, '额度内');
  assert.match(verdict.tip, /8,000/);
  assert.match(verdict.tip, /100,000/);
  assert.match(verdict.tip, /剩余 92,000/);
});

test('① 额度为 0（= 不限）：显示「额度不限」，不能显示「超 0」', () => {
  const verdict = creditVerdictOf(
    order({ over_credit_limit: false, credit_exposure: '50000.0000', credit_limit_snapshot: '0.0000' }),
  );
  assert.equal(verdict.over, false);
  assert.equal(verdict.label, '额度不限');
  assert.match(verdict.tip, /50,000/);
  assert.match(verdict.tip, /不限/);
});

// ==================== ② 安全库存口径 ====================

test('② 安全库存留空/0/负数/非数字 → 提交 0（= 不预警），绝不当成「阈值 0」', () => {
  assert.equal(safetyStockOf(''), 0);
  assert.equal(safetyStockOf('   '), 0);
  assert.equal(safetyStockOf('0'), 0);
  assert.equal(safetyStockOf('-5'), 0);
  assert.equal(safetyStockOf('abc'), 0);
  assert.equal(safetyStockOf('50'), 50);
  assert.equal(safetyStockOf(' 12.5 '), 12.5);
});

test('② 回填去掉 PG 的尾零（50.0000 → 50），用户不会以为自己填错', () => {
  assert.equal(quantityText('50.0000'), '50');
  assert.equal(quantityText(12.5), '12.5');
  assert.equal(quantityText(''), '');
  assert.equal(quantityText(null), '');
});

test('② 校验：留空合法；负数/非数字报错（与后端 require_non_negative 同口径）', () => {
  const ok = { ...emptyMaterialForm('RAW'), material_code: 'M-1', material_name: '物料', unit_id: 'u-1' };
  assert.equal(validateMaterialForm({ ...ok, safety_stock: '' }, false).safety_stock, undefined);
  assert.equal(validateMaterialForm({ ...ok, safety_stock: '50' }, false).safety_stock, undefined);
  assert.match(String(validateMaterialForm({ ...ok, safety_stock: '-1' }, false).safety_stock), /负数/);
  assert.match(String(validateMaterialForm({ ...ok, safety_stock: 'x1' }, false).safety_stock), /数字/);
});

test('② 新建/编辑请求体都显式带 safety_stock（留空发 0，不靠后端缺省蒙对）', () => {
  const form = { ...emptyMaterialForm('RAW'), material_code: 'M-1', material_name: '物料', unit_id: 'u-1' };
  assert.equal(buildMaterialCreatePayload(form).safety_stock, 0);
  assert.equal(buildMaterialCreatePayload({ ...form, safety_stock: '20' }).safety_stock, 20);

  const row = {
    id: 'm-1',
    material_code: 'M-1',
    material_name: '物料',
    spec: null,
    unit_id: 'u-1',
    material_type: 'RAW' as const,
    default_warehouse_id: null,
    safety_stock: '50.0000',
    costing_method: 'MOVING_AVG',
    inventory_account_id: null,
    tax_rate: '0',
    status: 1,
    serial_required: false,
    remark: null,
    created_at: '2026-11-04T00:00:00Z',
    updated_at: '2026-11-04T00:00:00Z',
  };
  // 回填 → 再提交：值不丢（50.0000 → '50' → 50）
  assert.equal(materialFormFromRow(row).safety_stock, '50');
  assert.equal(buildMaterialUpdatePayload(materialFormFromRow(row), row).safety_stock, 50);
  // 用户清空 → 发 0（= 关掉提醒），不是「不发这个字段」（那样等于没改）
  assert.equal(
    buildMaterialUpdatePayload({ ...materialFormFromRow(row), safety_stock: '' }, row).safety_stock,
    0,
  );
});

test('② 物料页有安全库存输入框 + 口径提示文案', () => {
  const page = sourceOf('src/views/business/erp/material/index.vue');
  assert.match(page, /data-testid="mat-safety-stock"/, '物料弹窗必须有安全库存输入框');
  assert.match(page, /SAFETY_STOCK_HINT/, '提示文案要走常量（一处改、三处一致）');
  assert.match(SAFETY_STOCK_HINT, /留空|0/, '提示必须写明「留空/0 = 不提醒」');
});

// ==================== ③ 锁定量（预留） ====================

test('③ 库存页不再显示「锁定」列（locked_qty 恒 0，显示它等于骗人）', () => {
  const page = sourceOf('src/views/business/erp/inventory/index.vue');
  assert.doesNotMatch(
    page,
    /colKey:\s*'locked_qty'/,
    '两个 Tab 的列定义里都不许再有 locked_qty 列',
  );
  assert.doesNotMatch(
    page,
    /#locked_qty=/, 
    '插槽也一并撤掉（留着就是死代码，将来有人手快把它接回列定义）',
  );
  assert.match(
    page,
    /预留字段/,
    '页面必须有一句人话说明「锁定量是预留字段」，否则用户以为是 bug',
  );
});

test('③ 采购单页显示超信用额度标签 + 详情信用结论', () => {
  const page = sourceOf('src/views/business/erp/purchase/index.vue');
  assert.match(page, /purchase-over-credit-/, '列表要有超信用额度标签');
  assert.match(page, /purchase-detail-credit/, '详情要有信用结论（未评估/额度内/超限）');
});
