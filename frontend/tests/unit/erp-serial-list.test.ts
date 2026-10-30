/**
 * `serial-logic.ts` 单测（W4b-2）：状态映射 / 可流转判断 / 扫码结果合并。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖、无 DOM）。
 *
 * 为什么测 `.ts` 不测 `.vue`：用例跑在没有 DOM 的 node 里，「在库显示成什么颜色、
 * 这行还能不能出库、扫码命中的行怎么在列表里标出来、查询参数带不带空串」全在
 * `serial-logic.ts`；`.vue` 只负责渲染。这些一旦漂移，`vue-tsc` 抓不到、e2e 也只看得见
 * 「界面长什么样」。
 *
 * 断言口径照后端抄（不是照卡面草图）：
 * - 状态三值 / 状态机 = `backend/src/business/erp/serial_model.rs::ALL_STATUSES` + `can_transition`；
 * - 可写目标只有 `OUTBOUND` / `SCRAPPED` = `serial_dto.rs::WRITABLE_TARGETS`；
 * - 扫码未命中是 HTTP 404 + `code = 10002` = `serial_handler::not_found`。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { SerialRow } from '@/api/business/erp';
import {
  MAX_BATCH_COUNT,
  SERIAL_STATUS_META,
  applyScanHighlight,
  availableActions,
  buildBatchPayload,
  buildSerialQuery,
  canTransition,
  emptyBatchForm,
  emptySerialFilter,
  isSerialNotFound,
  serialActionHint,
  serialStatusLabel,
  serialStatusTheme,
  validateBatchForm,
} from '@/views/business/erp/serial/serial-logic';

/** 一条列表行（字段值抄自真接口出参：GET /api/business/erp/serial/page） */
function seedRow(overrides: Partial<SerialRow> = {}): SerialRow {
  return {
    id: '01a18cb6-73b8-76ac-9077-e05a52ec186e',
    serial_no: 'SN-20261030-000011',
    material_id: 'd1370003-0000-4000-8000-000000000004',
    warehouse_id: 'd1370001-0000-4000-8000-000000000002',
    location_id: null,
    batch_no: null,
    status: 'IN_STOCK',
    inbound_at: '2026-10-30T12:08:49.847153Z',
    outbound_at: null,
    source_type: 'PURCHASE',
    source_id: null,
    created_at: '2026-10-30T12:08:49.847153Z',
    updated_at: '2026-10-30T12:08:49.847153Z',
    ...overrides,
  };
}

// ==================== ① 状态映射 ====================

test('① 状态映射：IN_STOCK→在库/绿、OUTBOUND→已出库/蓝、SCRAPPED→已报废/灰', () => {
  assert.equal(serialStatusLabel('IN_STOCK'), '在库');
  assert.equal(serialStatusLabel('OUTBOUND'), '已出库');
  assert.equal(serialStatusLabel('SCRAPPED'), '已报废');

  // 配色走 TDesign 语义槽（success=绿 / primary=蓝 / default=灰），不写裸 hex
  assert.equal(serialStatusTheme('IN_STOCK'), 'success');
  assert.equal(serialStatusTheme('OUTBOUND'), 'primary');
  assert.equal(serialStatusTheme('SCRAPPED'), 'default');

  // 三个状态一个都不能漏（后端加了新状态时这条会红，提醒同步口径）
  assert.deepEqual(Object.keys(SERIAL_STATUS_META).sort(), ['IN_STOCK', 'OUTBOUND', 'SCRAPPED']);

  // 未知值原样回显 + 中性色：不吞数据、也不继承上一行颜色
  assert.equal(serialStatusLabel('LOCKED'), 'LOCKED');
  assert.equal(serialStatusTheme('LOCKED'), 'default');
});

// ==================== ② 可流转判断 ====================

test('② 可流转：IN_STOCK 可出库可报废、OUTBOUND 只能报废、SCRAPPED 都不能', () => {
  // 与后端 can_transition 逐条对齐
  assert.equal(canTransition('IN_STOCK', 'OUTBOUND'), true);
  assert.equal(canTransition('IN_STOCK', 'SCRAPPED'), true);
  assert.equal(canTransition('OUTBOUND', 'SCRAPPED'), true);

  assert.equal(canTransition('OUTBOUND', 'IN_STOCK'), false, '出库不能回库');
  assert.equal(canTransition('SCRAPPED', 'OUTBOUND'), false, '报废是终态');
  assert.equal(canTransition('SCRAPPED', 'IN_STOCK'), false);
  assert.equal(canTransition('IN_STOCK', 'IN_STOCK'), false, '原地流转后端也拒');

  // 按钮可用性：页面据此置灰
  assert.deepEqual(availableActions('IN_STOCK'), { outbound: true, scrap: true });
  assert.deepEqual(availableActions('OUTBOUND'), { outbound: false, scrap: true });
  assert.deepEqual(availableActions('SCRAPPED'), { outbound: false, scrap: false });

  // 灰掉的按钮必须有理由（tooltip 文案），否则用户只看到两个点不动的灰字
  assert.match(serialActionHint('OUTBOUND'), /只能报废/);
  assert.match(serialActionHint('SCRAPPED'), /终态/);
});

// ==================== ③ 扫码结果合并 ====================

test('③ 扫码结果合并：命中行标 scanned=true，其余 false；不在本页时 hitOnPage=false', () => {
  const rows = [
    seedRow({ id: 'id-a', serial_no: 'SN-20261030-000001', status: 'OUTBOUND' }),
    seedRow({ id: 'id-b', serial_no: 'SN-20261030-000002' }),
    seedRow({ id: 'id-c', serial_no: 'SN-20261030-000003' }),
  ];

  // 命中第 2 行
  const hit = applyScanHighlight(rows, 'id-b');
  assert.equal(hit.hitOnPage, true, '命中行在本页');
  assert.deepEqual(
    hit.list.map((row) => [row.id, row.scanned]),
    [
      ['id-a', false],
      ['id-b', true],
      ['id-c', false],
    ],
  );
  // 原数组不能被就地改写（历史扫码态不能污染 rows.value）
  assert.equal(rows.some((row) => 'scanned' in row), false);

  // 扫到第 3 页的号：全部 false，页面据此提示「已按该号筛选」而不是静默什么都不发生
  const offPage = applyScanHighlight(rows, 'id-from-page-3');
  assert.equal(offPage.hitOnPage, false);
  assert.deepEqual(
    offPage.list.map((row) => row.scanned),
    [false, false, false],
  );

  // 没扫过（null）→ 一个都不标
  assert.equal(applyScanHighlight(rows, null).hitOnPage, false);

  // 404 / 10002 要能被识别成「未找到」，不是普通报错
  assert.equal(isSerialNotFound({ code: 10002, message: '序列号 X 不存在或已删除' }), true);
  assert.equal(isSerialNotFound({ message: '序列号 SN-X 不存在或已删除' }), true);
  assert.equal(isSerialNotFound({ code: 500, message: '服务器内部错误' }), false);
  assert.equal(isSerialNotFound(null), false);
});

// ==================== 附：查询参数 / 批量生成（同一文件的既有约定，一并钉住） ====================

test('筛选参数：空值整键缺席，序列号去空白，状态 / 物料 / 仓库按选中带上', () => {
  // 全空：只发分页参数（后端把 ?serial_no= 当「等于空串」过滤，发了就 0 行）
  assert.deepEqual(buildSerialQuery(emptySerialFilter(), 1), { page: 1, page_size: 20 });

  const query = buildSerialQuery(
    { material_id: 'm-1', warehouse_id: 'w-1', status: 'IN_STOCK', serial_no: '  SN-20261030-0000  ' },
    2,
    50,
  );
  assert.deepEqual(query, {
    page: 2,
    page_size: 50,
    material_id: 'm-1',
    warehouse_id: 'w-1',
    status: 'IN_STOCK',
    serial_no: 'SN-20261030-0000',
  });
});

test('批量生成表单：物料 / 仓库必填、数量 1..1000、请求体空值不发', () => {
  const empty = validateBatchForm(emptyBatchForm());
  assert.equal(empty.material_id !== undefined, true);
  assert.equal(empty.warehouse_id !== undefined, true, '仓库必填是不变式①，不是可选项');

  assert.deepEqual(
    validateBatchForm({ material_id: 'm-1', warehouse_id: 'w-1', count: 10, source_type: '', batch_no: '' }),
    {},
  );
  assert.equal(validateBatchForm({ ...emptyBatchForm(), material_id: 'm', warehouse_id: 'w', count: 0 }).count !== undefined, true);
  assert.equal(
    validateBatchForm({ ...emptyBatchForm(), material_id: 'm', warehouse_id: 'w', count: MAX_BATCH_COUNT + 1 }).count
      !== undefined,
    true,
  );

  assert.deepEqual(
    buildBatchPayload({ material_id: 'm-1', warehouse_id: 'w-1', count: 5, source_type: '', batch_no: '  ' }),
    { material_id: 'm-1', warehouse_id: 'w-1', count: 5 },
  );
  assert.deepEqual(
    buildBatchPayload({ material_id: 'm-1', warehouse_id: 'w-1', count: 5, source_type: 'PURCHASE', batch_no: 'B001' }),
    { material_id: 'm-1', warehouse_id: 'w-1', count: 5, source_type: 'PURCHASE', batch_no: 'B001' },
  );
});
