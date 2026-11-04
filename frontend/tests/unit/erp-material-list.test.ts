/**
 * `material-query.ts` 单测（W4b-1）：一物一码显示 / 筛选参数 / 分页参数 / 请求体。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * 为什么测 `.ts` 不测 `.vue`：用例跑在没有 DOM 的 node 里，「一物一码怎么显示、query 里
 * 到底带不带 serial_required、分页参数对不对、PUT 会不会偷偷带上 material_code」
 * 全在 `material-query.ts`；`.vue` 只负责渲染。这些一旦漂移，`vue-tsc` 抓不到、
 * e2e 也只看得见「界面长什么样」。
 *
 * 断言里的口径都照后端抄：
 * - 出参字段 = `backend/src/business/erp/dto.rs::MaterialRes`（**没有** category_id / unit_name / is_active）；
 * - 分类 = `dto::MATERIAL_TYPES`（RAW / SEMI / FINISHED / SERVICE）；
 * - 编辑请求体 = `dto::UpdateMaterialReq`（**没有** material_code）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { MaterialRow, UnitRow } from '@/api/business/erp';
import {
  COSTING_METHOD_HINT,
  COSTING_METHOD_OPTIONS,
  MATERIAL_PAGE_SIZE,
  buildMaterialCreatePayload,
  buildMaterialQuery,
  buildMaterialUpdatePayload,
  clampPage,
  costingMethodLabel,
  costingMethodOptions,
  emptyMaterialFilter,
  emptyMaterialForm,
  hasFieldError,
  isSerialRequired,
  materialFormFromRow,
  materialStatusLabel,
  materialTypeLabel,
  serialFilterFlag,
  serialRequiredLabel,
  unitNameOf,
  unitOptions,
  validateMaterialForm,
} from '@/views/business/erp/material/material-query';

/** 一条种子物料（V0137 的 M-W1A-004 成品D，字段值抄自真接口出参） */
function seedRow(overrides: Partial<MaterialRow> = {}): MaterialRow {
  return {
    id: 'd1370003-0000-4000-8000-000000000004',
    material_code: 'M-W1A-004',
    material_name: '成品D（种子）',
    spec: '整机',
    unit_id: 'd1370000-0000-4000-8000-000000000001',
    material_type: 'FINISHED',
    default_warehouse_id: 'd1370001-0000-4000-8000-000000000002',
    safety_stock: '0.0000',
    costing_method: 'MOVING_AVG',
    inventory_account_id: 'd1370004-0000-4000-8000-000000000001',
    tax_rate: '0.1300',
    status: 1,
    serial_required: true,
    remark: null,
    created_at: '2026-10-30T08:52:10.282882Z',
    updated_at: '2026-10-30T08:52:10.282882Z',
    ...overrides,
  };
}

/** 种子单位（V0137：PCS 个 / KG 千克 / BOX 箱） */
const UNITS: UnitRow[] = [
  { id: 'u-pcs', unit_code: 'PCS', unit_name: '个', is_base: true, status: 1 },
  { id: 'u-kg', unit_code: 'KG', unit_name: '千克', is_base: false, status: 1 },
];

// ① 一物一码判断与显示
test('serial_required 显示：true → 「★是」，false → 「否」', () => {
  assert.equal(serialRequiredLabel(true), '★是');
  assert.equal(serialRequiredLabel(false), '否');

  // 严格判定：只有 true / 1 / 'true' 算「是」，0、'0'、null、undefined 一律不算
  assert.equal(serialRequiredLabel(1), '★是');
  assert.equal(serialRequiredLabel(0), '否');
  assert.equal(serialRequiredLabel(null), '否');
  assert.equal(serialRequiredLabel(undefined), '否');

  assert.equal(isSerialRequired(true), true);
  assert.equal(isSerialRequired('true'), true);
  assert.equal(isSerialRequired(0), false);
  assert.equal(isSerialRequired('false'), false);
});

// ② 筛选参数组装：serial_required 是**三态**，全部时整键缺席
test('筛选参数：serial_required=true 带 true；all 时整键缺席（不是带 undefined / 空串）', () => {
  const base = emptyMaterialFilter();
  assert.equal(base.serial_required, 'all', '默认必须是「全部」');

  const yes = buildMaterialQuery({ ...base, serial_required: 'true' });
  assert.equal(yes.serial_required, true);
  assert.ok('serial_required' in yes);

  const no = buildMaterialQuery({ ...base, serial_required: 'false' });
  assert.equal(no.serial_required, false);
  assert.ok('serial_required' in no);

  const all = buildMaterialQuery({ ...base, serial_required: 'all' });
  assert.equal('serial_required' in all, false, '「全部」不许出现在 query 里');
  assert.equal(serialFilterFlag('all'), undefined);
  assert.equal(serialFilterFlag('true'), true);
  assert.equal(serialFilterFlag('false'), false);
});

// ② 附：关键词 trim + 空串不进参数（后端把 ?keyword= 当等于空串过滤，会查出 0 行）
test('筛选参数：keyword 先 trim，空串 / 全空格不发；分类空串不发', () => {
  const base = emptyMaterialFilter();

  assert.equal('keyword' in buildMaterialQuery({ ...base, keyword: '   ' }), false);
  assert.equal(buildMaterialQuery({ ...base, keyword: '  成品D  ' }).keyword, '成品D');
  assert.equal('material_type' in buildMaterialQuery({ ...base, material_type: '' }), false);
  assert.equal(buildMaterialQuery({ ...base, material_type: 'FINISHED' }).material_type, 'FINISHED');

  // 三个筛选一起上：都进 query
  const combined = buildMaterialQuery(
    { keyword: 'M-W1A', material_type: 'FINISHED', serial_required: 'true' },
    2,
    20,
  );
  assert.deepEqual(combined, {
    page: 2,
    page_size: 20,
    keyword: 'M-W1A',
    material_type: 'FINISHED',
    serial_required: true,
  });
});

// ③ 分页参数：page / page_size 正确且都是正整数
test('分页参数：page / page_size 正确（默认 1 / 10，非法值夹回合法值）', () => {
  const base = emptyMaterialFilter();

  assert.deepEqual(buildMaterialQuery(base), { page: 1, page_size: MATERIAL_PAGE_SIZE });
  assert.equal(MATERIAL_PAGE_SIZE, 10);

  assert.deepEqual(buildMaterialQuery(base, 3, 20), { page: 3, page_size: 20 });
  // 0 / 负数 / 小数 / NaN → 回到 1 或默认值，绝不把 page=0 发给后端
  assert.equal(buildMaterialQuery(base, 0).page, 1);
  assert.equal(buildMaterialQuery(base, -5).page, 1);
  assert.equal(buildMaterialQuery(base, 2.9).page, 2);
  assert.equal(buildMaterialQuery(base, Number.NaN).page, 1);
});

// ③ 附：页码夹紧 —— 筛选变窄后别停在不存在的页码上
test('页码夹紧：total=5 / size=10 时第 3 页回到第 1 页', () => {
  assert.equal(clampPage(3, 5, 10), 1);
  assert.equal(clampPage(1, 5, 10), 1);
  assert.equal(clampPage(2, 25, 10), 2);
  assert.equal(clampPage(9, 25, 10), 3);
  assert.equal(clampPage(1, 0, 10), 1);
  assert.equal(clampPage(0, 0, 10), 1);
});

// ④ 编辑请求体：不带 material_code；一物一码没变就不发
test('编辑请求体：无 material_code；serial_required 未改则缺席', () => {
  const row = seedRow();
  const form = materialFormFromRow(row);
  assert.equal(form.serial_required, true, '回填：一物一码物料开关必须是开');
  assert.equal(form.status, 1);

  const payload = buildMaterialUpdatePayload({ ...form, material_name: '成品D-改名' }, row);
  assert.equal(payload.material_name, '成品D-改名');
  assert.equal('material_code' in payload, false, 'UpdateMaterialReq 里没有 material_code');
  assert.equal('serial_required' in payload, false, '开关没动就不许发（后端缺席 = 不改）');
  assert.equal(payload.status, 1);

  // 真改了才发（防御将来放开置灰）
  const flipped = buildMaterialUpdatePayload({ ...form, serial_required: false }, row);
  assert.equal(flipped.serial_required, false);
});

// ⑤ 新建请求体：编码必填、serial_required 显式带上、成本法显式带上、空规格 → null
test('新建请求体：serial_required / costing_method 显式带上（不靠后端缺省）+ 空规格为 null', () => {
  const form = { ...emptyMaterialForm('FINISHED'), material_code: 'M-W1A-006', material_name: '成品F', unit_id: 'u-pcs' };

  const plain = buildMaterialCreatePayload(form);
  assert.deepEqual(plain, {
    material_code: 'M-W1A-006',
    material_name: '成品F',
    material_type: 'FINISHED',
    unit_id: 'u-pcs',
    spec: null,
    // P2-2 ②：安全库存显式带上（留空 = 0 = 不提醒；不靠后端缺省蒙对）
    safety_stock: 0,
    costing_method: 'MOVING_AVG',
    serial_required: false,
    status: 1,
    remark: null,
  });

  const serial = buildMaterialCreatePayload({ ...form, serial_required: true });
  assert.equal(serial.serial_required, true);

  // 表单里成本法被人为写成空串时兜底 MOVING_AVG（不许把空值发给后端）
  assert.equal(buildMaterialCreatePayload({ ...form, costing_method: '' }).costing_method, 'MOVING_AVG');
});

// ⑤ 附：成本法收敛（V0201 / ARCH-3 DC-10）—— 下拉只有一项 + 历史值兜底 + 编辑不发
test('成本法：下拉只有 MOVING_AVG；历史值作为禁用项兜底；编辑请求体不带 costing_method', () => {
  assert.deepEqual(COSTING_METHOD_OPTIONS, [{ label: '移动加权平均（MOVING_AVG）', value: 'MOVING_AVG' }]);
  assert.match(COSTING_METHOD_HINT, /本版本仅支持移动加权平均（MOVING_AVG）/);
  assert.match(COSTING_METHOD_HINT, /路线图/);

  // 正常值：只有一项，且可点（disabled 不为 true）
  const normal = costingMethodOptions('MOVING_AVG');
  assert.equal(normal.length, 1);
  assert.equal(normal[0].value, 'MOVING_AVG');
  assert.notEqual(normal[0].disabled, true);

  // 老数据 FIFO：下拉必须能显示出「这行原本是 FIFO」，且不许改选
  const legacy = costingMethodOptions('FIFO');
  assert.equal(legacy.length, 2);
  assert.equal(legacy[0].value, 'FIFO');
  assert.equal(legacy[0].disabled, true);
  assert.match(legacy[0].label, /历史值，本版本不支持/);
  assert.equal(legacy[1].value, 'MOVING_AVG');

  // 空值 / null：不给兜底项（避免出现空白选项）
  assert.equal(costingMethodOptions('').length, 1);
  assert.equal(costingMethodOptions(null).length, 1);

  assert.equal(costingMethodLabel('MOVING_AVG'), '移动加权平均（MOVING_AVG）');
  assert.equal(costingMethodLabel('FIFO'), 'FIFO', '未知值原样透出，不吞数据');
  assert.equal(costingMethodLabel(null), '—');

  // 表单回填 + 编辑请求体：成本法锁定，不随编辑重发（发了历史值只会换 400）
  const row = seedRow({ costing_method: 'FIFO' });
  const form = materialFormFromRow(row);
  assert.equal(form.costing_method, 'FIFO', '回填保留行上的原值，用户看得见');
  const payload = buildMaterialUpdatePayload(form, row);
  assert.equal('costing_method' in payload, false, '写死字段不许重发');
  assert.equal(emptyMaterialForm().costing_method, 'MOVING_AVG', '新建缺省 = 唯一可选值');
});

// ⑤ 附：校验（编码 ≤40 / 名称 ≤120 / 单位必填；编辑不校验编码）
test('表单校验：编码 / 名称 / 单位，编辑态不校验编码', () => {
  const empty = validateMaterialForm(emptyMaterialForm(), false);
  assert.equal(hasFieldError(empty), true);
  assert.ok(empty.material_code);
  assert.ok(empty.material_name);
  assert.ok(empty.unit_id);

  const ok = validateMaterialForm({ ...emptyMaterialForm(), material_code: 'M-1', material_name: 'A', unit_id: 'u' }, false);
  assert.equal(hasFieldError(ok), false);

  const tooLong = validateMaterialForm(
    { ...emptyMaterialForm(), material_code: 'X'.repeat(41), material_name: 'Y'.repeat(121), unit_id: 'u' },
    false,
  );
  assert.match(String(tooLong.material_code), /40/);
  assert.match(String(tooLong.material_name), /120/);

  // 编辑：编码只读，不校验它
  const editEmptyCode = validateMaterialForm(
    { ...emptyMaterialForm(), material_code: '', material_name: 'A', unit_id: 'u' },
    true,
  );
  assert.equal(hasFieldError(editEmptyCode), false);
});

// ⑥ 单位译名 / 分类文案 / 状态文案（列表三列）
test('列表文案：单位译名、分类中文、状态', () => {
  assert.equal(unitNameOf('u-pcs', UNITS), '个');
  assert.equal(unitNameOf('u-missing', UNITS), '—');
  assert.equal(unitNameOf(null, UNITS), '—');

  const options = unitOptions(UNITS);
  assert.deepEqual(options[0], { label: '个（PCS）', value: 'u-pcs' });

  assert.equal(materialTypeLabel('RAW'), '原材料');
  assert.equal(materialTypeLabel('SEMI'), '半成品');
  assert.equal(materialTypeLabel('FINISHED'), '产成品');
  assert.equal(materialTypeLabel('SERVICE'), '服务');
  // ERP-S1（V0187）新增的三类：现在有中文名，不再靠「原样透出」兜底
  assert.equal(materialTypeLabel('PACKAGING'), '包装物');
  assert.equal(materialTypeLabel('CONSUMABLE'), '低值易耗品');
  assert.equal(materialTypeLabel('OFFICE'), '办公用品');
  assert.equal(materialTypeLabel(null), '—');
  // 后端将来再加枚举：原样透出，不吞
  assert.equal(materialTypeLabel('UNKNOWN_TYPE'), 'UNKNOWN_TYPE');

  assert.equal(materialStatusLabel(1), '启用');
  assert.equal(materialStatusLabel(0), '停用');
});
