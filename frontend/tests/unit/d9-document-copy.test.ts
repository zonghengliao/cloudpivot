/**
 * D9「单据复制」前端纯逻辑单测。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖、无 DOM）。
 *
 * 复制的**语义在服务端**（新单号 / DRAFT / 清关联 / 取号走 biz_seq），前端只承担两件事，
 * 这两件都钉在纯函数上（`vue-tsc` 抓不到文案漂移，e2e 又只看得见界面）：
 *
 * 1. **提示语**：采购单被清空（收货单）/ 序列号不复制（调拨单）时，必须给用户一句解释，
 *    否则「复制丢数据」的误报会直接打到运维；
 * 2. **回填**：`detailToForm(复制出来的详情)` 必须把明细原样带进编辑表单，
 *    并且带出 `id`（否则点保存会**再建一张**而不是改这张草稿 —— 连点就多单了）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { detailToForm as stockInDetailToForm } from '@/views/business/erp/stockin/stockin-logic';
import { stockinCopyNotice } from '@/views/business/erp/stockin/stockin-logic';
import {
  detailToForm as transferDetailToForm,
  transferCopyNotice,
} from '@/views/business/erp/transfer/transfer-logic';

/** 一张复制出来的收货单详情（后端响应形状：`header` 包一层 + `items`） */
function stockInDetail(overrides: Record<string, unknown> = {}) {
  return {
    header: {
      id: 'stock-in-new',
      in_no: 'IN-20261101-000009',
      in_type: 'PURCHASE',
      source_order_id: null,
      source_order_no: null,
      supplier_id: 'sup-1',
      warehouse_id: 'wh-1',
      in_date: '2026-11-01',
      status: 'DRAFT',
      status_label: '草稿',
      currency_code: 'CNY',
      total_qty: '10',
      total_amount: '20',
      total_tax: '0',
      total_with_tax: '20',
      invoice_received: true,
      voucher_id: null,
      payable_id: null,
      remark: '复制源单备注',
      posted_at: null,
      posted_by: null,
      cancelled_at: null,
      cancelled_by: null,
      cancel_reason: null,
      approval_instance_id: null,
      create_by: null,
      created_at: '2026-11-01T00:00:00Z',
      updated_at: '2026-11-01T00:00:00Z',
      ...overrides,
    },
    items: [
      {
        id: 'item-1',
        stock_in_id: 'stock-in-new',
        line_no: 1,
        source_item_id: null,
        material_id: 'mat-1',
        unit_id: 'unit-1',
        qty: '10',
        price: '2',
        price_incl: '2',
        tax_rate: '0',
        amount: '20',
        tax_amount: '0',
        with_tax_amount: '20',
        cost_price: '0',
        location_id: null,
        lot_no: null,
        remark: null,
      },
    ],
    serial_count: 0,
    summary: '',
  };
}

/** 复制出来的调拨单详情（后端 `TransferDetail`：单头字段在顶层 + `items`） */
function transferDetail(serialCount: number) {
  return {
    id: 'transfer-new',
    transfer_no: 'TK-20261101-000003',
    from_warehouse_id: 'wh-1',
    to_warehouse_id: 'wh-2',
    status: 'DRAFT',
    transfer_date: '2026-11-01',
    reason: '复制源单原因',
    remark: '',
    total_qty: '7',
    items: [
      {
        id: 'line-1',
        transfer_id: 'transfer-new',
        line_no: 1,
        material_id: 'mat-1',
        qty: '7',
        shipped_qty: '0',
        received_qty: '0',
        serials:
          serialCount > 0
            ? Array.from({ length: serialCount }, (_, index) => ({
                id: `link-${index}`,
                serial_id: `serial-${index}`,
              }))
            : [],
      },
    ],
  };
}

test('D9 收货单：复制带出采购单 → 提示「请重新关联采购单」', () => {
  const notice = stockinCopyNotice({
    in_no: 'IN-20261001-000001',
    source_order_id: 'po-1',
    source_order_no: 'PO-20261001-000001',
  });
  assert.match(notice, /IN-20261001-000001/);
  assert.match(notice, /来源采购单已清空/);
  assert.match(notice, /重新关联采购单/);
});

test('D9 收货单：无源直收的单子复制 → 不提示（避免无意义打扰）', () => {
  assert.equal(
    stockinCopyNotice({ in_no: 'IN-20261001-000002', source_order_id: null, source_order_no: null }),
    '',
  );
  assert.equal(stockinCopyNotice({ in_no: 'IN-20261001-000003' }), '');
});

test('D9 收货单：回填表单带 id（保存走改这张草稿，不会又建一张）', () => {
  const detail = stockInDetail();
  const form = stockInDetailToForm(detail.header as never, detail.items as never);
  assert.equal(form.id, 'stock-in-new', 'form.id 为空 → 保存会再建一张，连点就多单');
  assert.equal(form.supplier_id, 'sup-1');
  assert.equal(form.warehouse_id, 'wh-1');
  assert.equal(form.remark, '复制源单备注');
  assert.equal(form.items.length, 1);
  assert.equal(form.items[0].material_id, 'mat-1');
  assert.equal(form.items[0].qty, 10);
  // 采购单关联已由后端清空 → 表单里也必须是空
  assert.equal(form.purchase_order_id, '');
});

test('D9 调拨单：原单带过序列号 → 提示「序列号不会复制」', () => {
  const notice = transferCopyNotice({ transfer_no: 'TK-20261001-000001', items: transferDetail(2).items });
  assert.match(notice, /TK-20261001-000001/);
  assert.match(notice, /序列号属于实物/);
});

test('D9 调拨单：原单没有序列号（数量物料）→ 不提示', () => {
  assert.equal(transferCopyNotice({ transfer_no: 'TK-20261001-000002', items: transferDetail(0).items }), '');
  assert.equal(transferCopyNotice({ transfer_no: 'TK-20261001-000003' }), '');
});

test('D9 调拨单：复制出来的详情回填后明细一致、id 指向新单', () => {
  const form = transferDetailToForm(transferDetail(0) as never, {});
  assert.equal(form.from_warehouse_id, 'wh-1');
  assert.equal(form.to_warehouse_id, 'wh-2');
  assert.equal(form.reason, '复制源单原因');
  assert.equal(form.transfer_date, '2026-11-01');
  assert.equal(form.items.length, 1);
  assert.equal(form.items[0].material_id, 'mat-1');
  assert.equal(form.items[0].qty, 7);
  // 后端不复制序列号 → 回填后也必须为空（否则会拿原单号码去占用）
  assert.deepEqual(form.items[0].serial_ids, []);
});
