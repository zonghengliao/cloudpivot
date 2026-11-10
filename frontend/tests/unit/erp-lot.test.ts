/**
 * 批次管理页纯逻辑单测（COST-B1-12 / ARCH-9 §5.2「批次主数据」· §6.1 #6）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * 测 `.ts` 不测 `.vue`：用例跑在没有 DOM 的 node 里，而「必填缺哪个 / 编辑发哪些字段 /
 * 筛选怎么拼参数」全在 `lot-logic.ts`；`.vue` 只负责画。这些口径一旦漂移
 * （整表回传 PUT、把 `material_id` 塞进编辑报文）`vue-tsc` 抓不到。
 *
 * 断言口径与后端逐字对齐：
 * - 值域：`ck_biz_lot_source` / `ck_biz_lot_qc`；状态是 **数字** 1/0（DDL SMALLINT）
 * - 长度：`lot_no VARCHAR(64)`
 * - 重复码：`error_code::ERP_LOT_NO_DUPLICATE = 66056`
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  LOT_NO_DUPLICATE_CODE,
  LOT_NO_MAX,
  LOT_PAGE_SIZE,
  LOT_PERMISSIONS,
  LOT_STATUS_DISABLED,
  LOT_STATUS_ENABLED,
  QC_STATUS_OPTIONS,
  SOURCE_TYPE_OPTIONS,
} from '../../src/api/business/erp/lot-kinds.ts';
import {
  buildLotQuery,
  emptyLotFilter,
  emptyLotForm,
  errorText,
  formFromRow,
  isDuplicateLotNoError,
  isEmptyUpdate,
  isLotFiltered,
  isLotFormValid,
  lotStatusLabel,
  lotStatusTheme,
  qcStatusLabel,
  qcStatusTheme,
  shortId,
  sourceTypeLabel,
  toCreatePayload,
  toUpdatePayload,
  validateLotForm,
  type LotFilterState,
  type LotFormState,
} from '../../src/views/business/erp/lot/lot-logic.ts';

const HERE = dirname(fileURLToPath(import.meta.url));

/** 一个填得完全合法的批次表单（各用例只改一处） */
function validForm(): LotFormState {
  const form = emptyLotForm(new Date('2026-11-10T02:00:00Z'));
  form.lot_no = 'LOT-A1';
  form.material_id = 'mat-1';
  return form;
}

// ==================== 词表即契约 ====================

test('值域与迁移里的 CHECK 逐字一致', () => {
  assert.deepEqual(
    SOURCE_TYPE_OPTIONS.map((item) => item.value),
    ['PURCHASE', 'PRODUCTION', 'OPENING', 'MANUAL'],
  );
  assert.deepEqual(
    QC_STATUS_OPTIONS.map((item) => item.value),
    ['NA', 'PENDING', 'PASSED', 'REJECTED'],
  );
  // 状态是数字：1 启用 / 0 停用（不是布尔）
  assert.equal(LOT_STATUS_ENABLED, 1);
  assert.equal(LOT_STATUS_DISABLED, 0);
  assert.equal(LOT_NO_MAX, 64, 'lot_no VARCHAR(64)');
  assert.equal(LOT_PAGE_SIZE, 20);
  assert.equal(LOT_NO_DUPLICATE_CODE, 66056);
});

test('权限码与 V0378 的菜单 / 按钮种子一致（且没有 delete —— 后端没这接口）', () => {
  assert.equal(LOT_PERMISSIONS.list, 'business:erp:lot:list');
  assert.equal(LOT_PERMISSIONS.create, 'business:erp:lot:create');
  assert.equal(LOT_PERMISSIONS.update, 'business:erp:lot:update');
  assert.equal('remove' in LOT_PERMISSIONS, false, '设计 §5.3 只有 3 个码');
});

// ==================== 表单 ====================

test('空表单：来源 MANUAL、质检 NA、启用、生产日期默认今天（CST）', () => {
  const form = emptyLotForm(new Date('2026-11-10T02:00:00Z'));
  assert.equal(form.id, null);
  assert.equal(form.lot_no, '');
  assert.equal(form.material_id, '');
  assert.equal(form.source_type, 'MANUAL');
  assert.equal(form.qc_status, 'NA');
  assert.equal(form.status, LOT_STATUS_ENABLED);
  assert.equal(form.prod_date, '2026-11-10', 'UTC 02:00 = CST 10:00，同一天');
  assert.equal(form.expire_date, '');
});

test('校验：批号与物料必填、批号超长、失效早于生产都被拦', () => {
  assert.equal(isLotFormValid(validForm()), true);

  const noNo = validForm();
  noNo.lot_no = '   ';
  assert.match(validateLotForm(noNo).lot_no, /批号不能为空/);

  const noMaterial = validForm();
  noMaterial.material_id = '';
  assert.match(validateLotForm(noMaterial).material_id, /请选择物料/);

  const tooLong = validForm();
  tooLong.lot_no = 'L'.repeat(LOT_NO_MAX + 1);
  assert.match(validateLotForm(tooLong).lot_no, /64/);

  const inverted = validForm();
  inverted.prod_date = '2026-11-10';
  inverted.expire_date = '2026-11-09';
  assert.match(validateLotForm(inverted).expire_date, /不能早于生产日期/);

  // 同一天合法（后端 CHECK 是 expire >= prod）
  const sameDay = validForm();
  sameDay.prod_date = '2026-11-10';
  sameDay.expire_date = '2026-11-10';
  assert.equal(isLotFormValid(sameDay), true);
});

test('建单报文：批号 trim、空日期 / 空备注发 null（不是空串）', () => {
  const form = validForm();
  form.lot_no = '  LOT-A1  ';
  form.expire_date = '';
  form.remark = '  ';

  const payload = toCreatePayload(form);
  assert.equal(payload.lot_no, 'LOT-A1');
  assert.equal(payload.material_id, 'mat-1');
  assert.equal(payload.source_type, 'MANUAL');
  assert.equal(payload.expire_date, null);
  assert.equal(payload.remark, null);
  assert.equal(payload.status, LOT_STATUS_ENABLED);
  // material_id 在建单报文里（业务键的一半）
  assert.equal('material_id' in payload, true);
});

test('编辑报文只带真改过的字段（后端三态：缺失 = 不改）', () => {
  const original = validForm();
  const untouched = { ...original };
  assert.equal(isEmptyUpdate(toUpdatePayload(original, untouched)), true, '没改就不该发请求');

  const changed = { ...original, qc_status: 'REJECTED' as const };
  const payload = toUpdatePayload(original, changed);
  assert.deepEqual(payload, { qc_status: 'REJECTED' });

  // 可空字段从有值改成空 → 显式 null（清空）
  const withRemark = { ...original, remark: '首件' };
  const cleared = toUpdatePayload(withRemark, { ...withRemark, remark: '' });
  assert.deepEqual(cleared, { remark: null });

  // 只差空白不算改（trim 后比较）
  const spaced = { ...original, remark: '首件' };
  const sameText = toUpdatePayload(spaced, { ...spaced, remark: '  首件  ' });
  assert.equal(isEmptyUpdate(sameText), true);

  // material_id 永远不发（业务键不可变，后端入参里也没有）
  const movedMaterial = { ...original, material_id: 'mat-2' };
  assert.equal('material_id' in toUpdatePayload(original, movedMaterial), false);

  // 状态改了要发（1 → 0 停用）
  const disabled = { ...original, status: LOT_STATUS_DISABLED };
  assert.deepEqual(toUpdatePayload(original, disabled), { status: LOT_STATUS_DISABLED });
});

test('行 → 表单：日期 / 备注的 null 都变成空串，状态归一为 1/0', () => {
  const form = formFromRow({
    id: 'lot-1',
    lot_no: 'LOT-A1',
    material_id: 'mat-1',
    material_name: '钢轴',
    material_code: 'M-001',
    source_type: 'PURCHASE',
    source_doc_type: 'STOCK_IN',
    source_doc_id: null,
    source_doc_no: 'IN-20261101-000001',
    supplier_id: null,
    supplier_name: null,
    prod_date: '2026-11-01',
    expire_date: null,
    qc_status: 'PASSED',
    status: 0,
    remark: null,
    created_at: '2026-11-01T02:00:00Z',
    updated_at: '2026-11-01T02:00:00Z',
  });
  assert.equal(form.id, 'lot-1');
  assert.equal(form.expire_date, '');
  assert.equal(form.remark, '');
  assert.equal(form.status, LOT_STATUS_DISABLED);
  assert.equal(form.source_type, 'PURCHASE');
  assert.equal(form.qc_status, 'PASSED');
});

// ==================== 列表筛选 ====================

test('筛选：空条件只剩分页；有物料 / 状态时按后端字段名拼（status 是数字）', () => {
  const empty = buildLotQuery(emptyLotFilter(), 1);
  assert.deepEqual(empty, { page: 1, page_size: LOT_PAGE_SIZE });

  const filter: LotFilterState = {
    keyword: '  LOT  ',
    material_id: 'mat-1',
    source_type: 'PRODUCTION',
    qc_status: 'PASSED',
    status: String(LOT_STATUS_DISABLED),
  };
  const query = buildLotQuery(filter, 2, 50);
  assert.deepEqual(query, {
    page: 2,
    page_size: 50,
    keyword: 'LOT',
    material_id: 'mat-1',
    source_type: 'PRODUCTION',
    qc_status: 'PASSED',
    status: 0,
  });
  assert.equal(isLotFiltered(filter), true);
  assert.equal(isLotFiltered(emptyLotFilter()), false);
});

test('筛选：只按物料也能筛（本卡验收项）', () => {
  const filter = emptyLotFilter();
  filter.material_id = 'mat-9';
  const query = buildLotQuery(filter, 1);
  assert.equal(query.material_id, 'mat-9');
  assert.equal('keyword' in query, false, '空关键词不许发（后端会查出 0 行）');
});

// ==================== 译名 / 错误 ====================

test('译名与标签主题', () => {
  assert.equal(sourceTypeLabel('PRODUCTION'), '生产批');
  assert.equal(sourceTypeLabel('OPENING'), '期初');
  assert.equal(qcStatusLabel('REJECTED'), '不合格');
  assert.equal(qcStatusTheme('REJECTED'), 'danger');
  assert.equal(qcStatusTheme('PENDING'), 'warning');
  assert.equal(lotStatusLabel(1), '启用');
  assert.equal(lotStatusLabel(0), '停用');
  assert.equal(lotStatusTheme(0), 'default');
  assert.equal(shortId('018f4b2c-1111-7222-8333-444455556666'), '018f4b2c…');
});

test('错误文案原样带后端 msg（批号重复的物料 + 批号不能丢）', () => {
  const duplicate = {
    code: 66056,
    message: '批号「LOT-A1」在物料「M-001 钢轴」下已存在：同一物料不能有两个相同批号。',
  };
  const text = errorText(duplicate);
  assert.match(text, /LOT-A1/);
  assert.match(text, /M-001 钢轴/);
  assert.match(text, /66056/);
  assert.equal(isDuplicateLotNoError(duplicate), true);
  assert.equal(isDuplicateLotNoError({ code: 10001, message: 'x' }), false);
  assert.match(errorText(null), /接口没有返回说明/);
});

// ==================== 源码即契约 ====================

test('API 层：路径写全 /api、四个函数齐全、没有 deleteLot（后端无此路由）', () => {
  const source = readFileSync(resolve(HERE, '../../src/api/business/erp/lot.ts'), 'utf8');
  assert.match(source, /const BASE = '\/api\/business\/erp'/);
  for (const endpoint of ['`${BASE}/lot`', '`${BASE}/lot/options`', '`${BASE}/lot/${id}`']) {
    assert.ok(source.includes(endpoint), `缺少接口路径 ${endpoint}`);
  }
  for (const fn of ['fetchLotPage', 'fetchLotOptions', 'createLot', 'updateLot']) {
    assert.ok(source.includes(`export function ${fn}`), `缺少导出函数 ${fn}`);
  }
  assert.equal(source.includes('export function deleteLot'), false, '没有 DELETE 接口，不许导出');
});

test('路由已注册（漏了 = 点菜单进 404）', () => {
  const router = readFileSync(resolve(HERE, '../../src/router/index.ts'), 'utf8');
  assert.match(router, /path: 'business\/erp\/lot'/);
  assert.match(router, /views\/business\/erp\/lot\/index\.vue/);
  assert.match(router, /permission: 'business:erp:lot:list'/);
});
