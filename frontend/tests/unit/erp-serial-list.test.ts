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
  PARAM_INVALID_CODE,
  SERIAL_STATUS_META,
  applyScanHighlight,
  availableActions,
  batchGenerateErrorText,
  buildBatchPayload,
  buildSerialQuery,
  canTransition,
  defaultLocationIdOf,
  emptyBatchForm,
  emptySerialFilter,
  formatBatchAmount,
  formatBatchResultText,
  isSerialNotFound,
  locationAfterWarehouseSwitch,
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

test('批量生成表单：物料 / 仓库 / 成本单价必填、数量 1..1000、请求体空值不发', () => {
  const empty = validateBatchForm(emptyBatchForm());
  assert.equal(empty.material_id !== undefined, true);
  assert.equal(empty.warehouse_id !== undefined, true, '仓库必填是不变式①，不是可选项');
  assert.equal(empty.unit_cost !== undefined, true, '成本单价必填：批量生成 = 无来源入库，成本由操作人负责');

  assert.deepEqual(
    validateBatchForm(
      { material_id: 'm-1', warehouse_id: 'w-1', count: 10, unit_cost: '12.34', source_type: '', batch_no: '' },
    ),
    {},
  );
  assert.equal(validateBatchForm({ ...emptyBatchForm(), material_id: 'm', warehouse_id: 'w', count: 0 }).count !== undefined, true);
  assert.equal(
    validateBatchForm({ ...emptyBatchForm(), material_id: 'm', warehouse_id: 'w', count: MAX_BATCH_COUNT + 1 }).count
      !== undefined,
    true,
  );

  assert.deepEqual(
    buildBatchPayload(
      {
        material_id: 'm-1',
        warehouse_id: 'w-1',
        location_dict_id: '',
        count: 5,
        unit_cost: '12.34',
        source_type: '',
        batch_no: '  ',
      },
    ),
    { material_id: 'm-1', warehouse_id: 'w-1', count: 5, unit_cost: 12.34 },
  );
  assert.deepEqual(
    buildBatchPayload(
      {
        material_id: 'm-1',
        warehouse_id: 'w-1',
        location_dict_id: '',
        count: 5,
        unit_cost: '0.000001',
        source_type: 'PURCHASE',
        batch_no: 'B001',
      },
    ),
    { material_id: 'm-1', warehouse_id: 'w-1', count: 5, unit_cost: 0.000001, source_type: 'PURCHASE', batch_no: 'B001' },
  );
});

test('E2E-FIX-6 批量生成请求体带上入库库位（库位字典 id，与收货同口径）', () => {
  const base = {
    material_id: 'm-1',
    warehouse_id: 'w-1',
    count: 3,
    unit_cost: '10',
    source_type: '' as const,
    batch_no: '',
  };
  // ① 选了库位 → 发 location_dict_id（**不是** location_id：那是库位账行 id，前端拿不到）
  const withLocation = buildBatchPayload({ ...base, location_dict_id: ' loc-1 ' }) as Record<string, unknown>;
  assert.equal(withLocation.location_dict_id, 'loc-1');
  assert.equal('location_id' in withLocation, false);

  // ② 没选库位 → 整键缺席（后端按仓库默认库位兜底）
  const without = buildBatchPayload({ ...base, location_dict_id: '' }) as Record<string, unknown>;
  assert.equal('location_dict_id' in without, false);
});

test('E2E-FIX-6 库位下拉：默认带出仓库默认库位，切仓不覆盖用户已选库位', () => {
  const locations = [
    { id: 'loc-a', is_default: false, status: 1 },
    { id: 'loc-def', is_default: true, status: 1 },
    { id: 'loc-off', is_default: false, status: 0 },
  ];
  assert.equal(defaultLocationIdOf(locations), 'loc-def');
  // 默认库位停用 ⇒ 不带出（后端也只认启用的）
  assert.equal(defaultLocationIdOf([{ id: 'x', is_default: true, status: 0 }]), '');
  // 没有默认库位 ⇒ 留空，交给后端兜底，不编造
  assert.equal(defaultLocationIdOf([{ id: 'x', is_default: false, status: 1 }]), '');
  assert.equal(defaultLocationIdOf([]), '');

  // 切仓：旧选择不在新仓库列表里 → 换成新仓默认库位
  assert.equal(locationAfterWarehouseSwitch(locations, 'loc-other'), 'loc-def');
  // 旧选择仍在列表里 → 保留用户选择（不被默认值悄悄覆盖）
  assert.equal(locationAfterWarehouseSwitch(locations, 'loc-a'), 'loc-a');
  // 没选仓库（列表为空）→ 清空
  assert.equal(locationAfterWarehouseSwitch([], 'loc-a'), '');
});

test('批量生成表单：成本单价四条校验（空 / 非数字 / ≤0 / 7 位小数），口径照后端 parse_unit_cost', () => {
  // 缺单价：不能提交，且提示要写清「为什么必须填」（口径 = ADR ①′）
  const missing = validateBatchForm({ ...emptyBatchForm(), material_id: 'm', warehouse_id: 'w' });
  assert.match(missing.unit_cost ?? '', /请填写成本单价/);
  assert.match(missing.unit_cost ?? '', /期初\/其他入库/);

  const blank = validateBatchForm({ ...emptyBatchForm(), material_id: 'm', warehouse_id: 'w', unit_cost: '   ' });
  assert.match(blank.unit_cost ?? '', /请填写成本单价/);

  assert.equal(
    validateBatchForm({ ...emptyBatchForm(), material_id: 'm', warehouse_id: 'w', unit_cost: 'abc' }).unit_cost,
    '成本单价必须是数字',
  );
  assert.equal(
    validateBatchForm({ ...emptyBatchForm(), material_id: 'm', warehouse_id: 'w', unit_cost: '0' }).unit_cost,
    '成本单价必须大于 0',
  );
  assert.equal(
    validateBatchForm({ ...emptyBatchForm(), material_id: 'm', warehouse_id: 'w', unit_cost: '-1.5' }).unit_cost,
    '成本单价必须大于 0',
  );
  // 位数校验只数文本（12.3400000 浮点化之后等于 12.34，过一遍 Number() 就漏了）
  assert.equal(
    validateBatchForm({ ...emptyBatchForm(), material_id: 'm', warehouse_id: 'w', unit_cost: '12.3400000' }).unit_cost,
    '成本单价最多 6 位小数',
  );
  assert.equal(
    validateBatchForm({ ...emptyBatchForm(), material_id: 'm', warehouse_id: 'w', unit_cost: '12.340000' }).unit_cost,
    undefined,
  );
});

test('入库金额预览：数量 × 单价，4 位小数（与 biz_inventory.amount numeric(18,4) 一致）', () => {
  assert.equal(formatBatchAmount({ count: 10, unit_cost: '12.34' }), '123.4000');
  assert.equal(formatBatchAmount({ count: 5, unit_cost: '12.340000' }), '61.7000');
  assert.equal(formatBatchAmount({ count: 3, unit_cost: '0.1' }), '0.3000', '浮点误差不能漏到 4 位小数之外');
  assert.equal(formatBatchAmount({ count: 1, unit_cost: '0.000001' }), '0.0000', '单价精度 6 位、金额精度 4 位');
  // 非法输入宁可显示 —，也不能显示 NaN / Infinity
  assert.equal(formatBatchAmount({ count: 10, unit_cost: '' }), '—');
  assert.equal(formatBatchAmount({ count: 10, unit_cost: 'abc' }), '—');
  assert.equal(formatBatchAmount({ count: 10, unit_cost: '0' }), '—');
  assert.equal(formatBatchAmount({ count: 0, unit_cost: '12.34' }), '—');
});

test('批量生成失败文案：400/10001 的 msg 原样透出，拿不到 msg 才兜底', () => {
  // 报文逐字取自规格 §2.3（前端不许改写）
  const missingMsg = 'unit_cost 必填：批量生成按「期初/其他入库」记账，必须录成本单价（不含税）';
  assert.equal(batchGenerateErrorText({ code: PARAM_INVALID_CODE, message: missingMsg }), missingMsg);
  assert.equal(
    batchGenerateErrorText({
      code: PARAM_INVALID_CODE,
      message: 'unit_cost 必须 > 0（收到 0）：零成本入库会在成本账留下 qty>0/avg_cost=0 的行（巡检 C3），系统拒收',
    }),
    'unit_cost 必须 > 0（收到 0）：零成本入库会在成本账留下 qty>0/avg_cost=0 的行（巡检 C3），系统拒收',
  );
  // 网关 502 / 空响应体：没有 msg 可透出，给一句人话
  assert.equal(batchGenerateErrorText({ code: -1, message: '   ' }), '批量生成失败，请稍后重试');
  assert.equal(batchGenerateErrorText(undefined), '批量生成失败，请稍后重试');
});

test('批量生成成功摘要：金额 / 结存用后端回显的字符串（前端不重算）', () => {
  const text = formatBatchResultText({
    count: 5,
    period_key: '20261031',
    serial_nos: ['SN-20261031-000101', 'SN-20261031-000102', 'SN-20261031-000103', 'SN-20261031-000104'],
    unit_cost: '12.340000',
    amount: '61.7000',
    inventory: { qty: '5.0000', amount: '61.7000', avg_cost: '12.340000' },
  });
  assert.match(text, /生成了 5 个序列号（20261031）/);
  assert.match(text, /SN-20261031-000101 \/ SN-20261031-000102 \/ SN-20261031-000103 …/);
  assert.match(text, /本次入库 5 件 × 12\.340000 = 61\.7000 元/);
  assert.match(text, /账面结存 5\.0000 \/ 61\.7000 元（均价 12\.340000）/);

  // 老后端没有 amount/unit_cost：退化成「刷新后可见」，不显示 undefined
  const legacy = formatBatchResultText({ count: 2, period_key: '20261031', serial_nos: ['SN-1', 'SN-2'] });
  assert.match(legacy, /刷新列表可见/);
  assert.doesNotMatch(legacy, /undefined/);
  assert.doesNotMatch(legacy, /NaN/);
});
