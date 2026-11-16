/**
 * M2-17：安全库存设置页**页面**契约单测（批量设置 + 导入入口）。
 *
 * 运行：`cd frontend && pnpm test:unit`（`tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * # 两层断言
 *
 * 1. **纯逻辑**（`stock-policy-logic.ts`）：拼参（空筛选整键缺席）、批量展开（笛卡尔积 /
 *    去重 / 2000 格上限）、四列校验（`max > min` 严格不等式）、数量格式化（`Decimal` 出字符串、
 *    `null` 最高水位显示成 `—` 而不是 `0`）。这些是「把水位设错但界面看不出错」的入口。
 * 2. **源码即契约**（`index.vue` / `api/.../stock-policy.ts` / `router/index.ts` / 迁移 `V0431`
 *    / 后端 `impexp::builtin::erp_setting`）：真正会静默出错的是「两边常量漂了」——
 *    例如页面 `biz-type` 与后端定义不一致（下载模板 404 `60011`）、批量上限两边不同
 *    （页面放行 3000 格、后端 400）、路由 path 与迁移 `sys_menu.path` 不一致（点菜单 404）。
 *
 * 只读源码做子串 / 正则断言，不引 DOM、不引浏览器（与 `m212-inventory-aging-page.test.ts` 同款）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

import {
  MAX_BATCH_CELLS,
  PERM_STOCK_POLICY_DELETE,
  PERM_STOCK_POLICY_LIST,
  PERM_STOCK_POLICY_PAGE,
  PERM_STOCK_POLICY_SAVE,
  STOCK_POLICY_IMPORT_BIZ_TYPE,
  STOCK_POLICY_PAGE_SIZE,
  batchPreviewText,
  buildStockPolicyBatchPayload,
  buildStockPolicyQuery,
  buildStockPolicySavePayload,
  emptyStockPolicyBatchForm,
  emptyStockPolicyCellForm,
  emptyStockPolicyFilter,
  emptyText,
  formFromStockPolicyRow,
  formatQty,
  isStockPolicyFiltered,
  materialText,
  minQtyText,
  previewStockPolicyBatch,
  qtyText,
  statusHint,
  statusLabel,
  statusTheme,
  validateStockPolicyBatch,
  validateStockPolicyLevels,
  warehouseText,
} from '@/views/business/erp/stock-policy/stock-policy-logic';
import type { StockPolicyRow } from '@/api/business/erp/stock-policy';

const ROOT = resolve(process.cwd());

function sourceOf(relative: string): string {
  return readFileSync(join(ROOT, relative), 'utf8');
}

/** 取 `<template>` 段：脚本里的同名函数不算页面渲染契约 */
function templateOf(source: string): string {
  const start = source.indexOf('<template>');
  const end = source.lastIndexOf('</template>');
  assert.ok(start >= 0 && end > start, '页面必须有 <template> 段');
  return source.slice(start + '<template>'.length, end);
}

/** 去掉注释再断言实现约束：文档里为了解释「为什么」会提到被禁止的写法本身 */
function withoutComments(source: string): string {
  return source.replace(/\/\*[\s\S]*?\*\//g, '').replace(/(^|[^:])\/\/.*$/gm, '$1');
}

const PAGE_PATH = 'src/views/business/erp/stock-policy/index.vue';
const LOGIC_PATH = 'src/views/business/erp/stock-policy/stock-policy-logic.ts';
const API_PATH = 'src/api/business/erp/stock-policy.ts';
const ROUTER_PATH = 'src/router/index.ts';
const MIG_UP_PATH = '../backend/app/migrations/business/V0431__erp_stock_policy_page.up.sql';
const MIG_DOWN_PATH = '../backend/app/migrations/business/V0431__erp_stock_policy_page.down.sql';
const BACKEND_DTO_PATH = '../backend/app/src/business/erp/stock_policy/dto.rs';
const BACKEND_IMPORT_PATH = '../backend/app/src/business/impexp/builtin/erp_setting.rs';

const PAGE = sourceOf(PAGE_PATH);
const TPL = templateOf(PAGE);
const LOGIC = sourceOf(LOGIC_PATH);
const API = sourceOf(API_PATH);
const ROUTER = sourceOf(ROUTER_PATH);
const MIG_UP = sourceOf(MIG_UP_PATH);
const MIG_DOWN = sourceOf(MIG_DOWN_PATH);
const BACKEND_DTO = sourceOf(BACKEND_DTO_PATH);
const BACKEND_IMPORT = sourceOf(BACKEND_IMPORT_PATH);

// ==================== ① 拼参：空筛选整键缺席 ====================

test('列表拼参：全空 → 只发分页（后端把 ?material_id= 当非法 UUID → 400）', () => {
  assert.deepEqual(buildStockPolicyQuery(emptyStockPolicyFilter(), 1, STOCK_POLICY_PAGE_SIZE), {
    page: 1,
    page_size: STOCK_POLICY_PAGE_SIZE,
  });
});

test('列表拼参：空白关键字等于没填，不能发 ?keyword=', () => {
  const query = buildStockPolicyQuery({ ...emptyStockPolicyFilter(), keyword: '   ' }, 2, 20);
  assert.equal('keyword' in query, false);
  assert.equal(query.page, 2);
});

test('列表拼参：填了就原样带上（status 是字符串，与查询串同口径）', () => {
  const query = buildStockPolicyQuery(
    { keyword: ' 螺丝 ', material_id: 'm-1', warehouse_id: 'w-1', status: '0' },
    1,
    20,
  );
  assert.equal(query.keyword, '螺丝', '关键字两端空白要去掉');
  assert.equal(query.material_id, 'm-1');
  assert.equal(query.warehouse_id, 'w-1');
  assert.equal(query.status, '0');
});

test('列表拼参：页码 0 / 负数回落 1（后端 page=0 会静默回落，页面别先发个怪值）', () => {
  assert.equal(buildStockPolicyQuery(emptyStockPolicyFilter(), 0, 20).page, 1);
  assert.equal(buildStockPolicyQuery(emptyStockPolicyFilter(), -3, 20).page, 1);
});

test('筛选态判定：空白关键字不算筛过（空态文案要说「还没设过」）', () => {
  assert.equal(isStockPolicyFiltered(emptyStockPolicyFilter()), false);
  assert.equal(isStockPolicyFiltered({ ...emptyStockPolicyFilter(), keyword: '  ' }), false);
  assert.equal(isStockPolicyFiltered({ ...emptyStockPolicyFilter(), keyword: 'x' }), true);
  assert.equal(isStockPolicyFiltered({ ...emptyStockPolicyFilter(), status: '1' }), true);
  assert.equal(isStockPolicyFiltered({ ...emptyStockPolicyFilter(), material_id: 'm' }), true);
});

// ==================== ② 四列校验（max 必须严格大于 min） ====================

test('校验：最低水位必填（空 / 全空白都拦）', () => {
  assert.equal(validateStockPolicyLevels('', '', 1)?.field, 'min_qty');
  assert.equal(validateStockPolicyLevels('   ', '', 1)?.field, 'min_qty');
});

test('校验：最低水位负数 / 非数字 都拦，并说清是哪个框', () => {
  const negative = validateStockPolicyLevels('-5', '', 1);
  assert.equal(negative?.field, 'min_qty');
  assert.ok(negative?.message.includes('负数'), negative?.message);

  const nan = validateStockPolicyLevels('abc', '', 1);
  assert.equal(nan?.field, 'min_qty');
  assert.ok(nan?.message.includes('数字'), nan?.message);
});

test('校验：最高水位留空 = 不判超储 → 通过（空与 0 是两件事）', () => {
  assert.equal(validateStockPolicyLevels('100', '', 1), null);
  assert.equal(validateStockPolicyLevels('0', '  ', 1), null);
});

test('校验：max == min（空区间）与 max < min 都要拦住，点名最高水位', () => {
  const equal = validateStockPolicyLevels('100', '100', 1);
  assert.equal(equal?.field, 'max_qty');
  assert.ok(equal?.message.includes('严格大于'), equal?.message);

  const less = validateStockPolicyLevels('100', '99.9999', 1);
  assert.equal(less?.field, 'max_qty');
  assert.ok(less?.message.includes('严格大于'), less?.message);
});

test('校验：max > min（含 4 位小数刀口 0.0001）通过', () => {
  assert.equal(validateStockPolicyLevels('100', '100.0001', 1), null);
  assert.equal(validateStockPolicyLevels('0', '0.0001', 1), null);
});

test('校验：状态域外拦住（与后端 ck_biz_msp_status 同口径）', () => {
  const bad = validateStockPolicyLevels('10', '', 2 as unknown as number);
  assert.equal(bad?.field, null);
  assert.ok(bad?.message.includes('启用'), bad?.message);
});

// ==================== ③ 表单回填 / 提交载荷 ====================

const row = (over: Partial<StockPolicyRow> = {}): StockPolicyRow => ({
  id: 'r-1',
  material_id: 'm-1',
  material_code: 'M213-M1',
  material_name: '水位物料甲',
  spec: '规格 A',
  unit_name: '个',
  warehouse_id: 'w-1',
  warehouse_code: 'M213-WHA',
  warehouse_name: '水位仓甲',
  min_qty: '100.0000',
  max_qty: '500.0000',
  status: 1,
  remark: '备件仓保底',
  created_at: '2026-11-16T10:00:00Z',
  updated_at: '2026-11-16T10:00:00Z',
  ...over,
});

test('回填：数量去尾零，max 为 null → 空串（表单显示「未设」而不是 0）', () => {
  const form = formFromStockPolicyRow(row({ max_qty: null, min_qty: '100.5000', status: 0 }));
  assert.equal(form.min_qty, '100.5');
  assert.equal(form.max_qty, '');
  assert.equal(form.status, 0, '停用要回填成 0');
  assert.equal(form.remark, '备件仓保底');
});

test('回填：新建表单是空值 + 默认启用', () => {
  const form = emptyStockPolicyCellForm();
  assert.equal(form.min_qty, '');
  assert.equal(form.max_qty, '');
  assert.equal(form.status, 1);
});

test('载荷：max 留空 → null、备注空白 → null（覆盖成 NULL，不是空串）', () => {
  const payload = buildStockPolicySavePayload({
    material_id: 'm-1',
    warehouse_id: 'w-1',
    min_qty: ' 100 ',
    max_qty: '   ',
    status: 1,
    remark: '   ',
  });
  assert.equal(payload.min_qty, '100');
  assert.equal(payload.max_qty, null);
  assert.equal(payload.remark, null);
  assert.equal(payload.status, 1);
});

// ==================== ④ 批量展开（与后端 to_plan 同口径） ====================

test('批量展开：3 物料 × 2 仓库 = 6 格；顺序 = 入参顺序的笛卡尔积', () => {
  const preview = previewStockPolicyBatch(['m1', 'm2', 'm3'], ['w1', 'w2']);
  assert.equal(preview.cells, 6);
  assert.deepEqual(preview.materials, ['m1', 'm2', 'm3']);
  assert.deepEqual(preview.warehouses, ['w1', 'w2']);
  assert.equal(preview.duplicateMaterials, 0);
  assert.equal(preview.overLimit, false);
});

test('批量展开：重复 id 去重并计数；空白项丢弃且不算重复', () => {
  const preview = previewStockPolicyBatch(['m1', 'm1', '  m1  ', '', 'm2'], [' w1 ', 'w1', '  ']);
  assert.deepEqual(preview.materials, ['m1', 'm2']);
  assert.deepEqual(preview.warehouses, ['w1']);
  assert.equal(preview.duplicateMaterials, 2, 'm1 传了 3 次 → 去掉 2 个重复（空项不算）');
  assert.equal(preview.duplicateWarehouses, 1);
  assert.equal(preview.cells, 2);
});

test('批量展开：空列表 → 0 格、不超限', () => {
  const preview = previewStockPolicyBatch([], []);
  assert.equal(preview.cells, 0);
  assert.equal(preview.overLimit, false);
});

test('批量展开：上限 2000 格（2000 放行 / 2001 超限），与后端 MAX_BATCH_CELLS 同值', () => {
  assert.equal(MAX_BATCH_CELLS, 2000);
  const atLimit = Array.from({ length: MAX_BATCH_CELLS }, (_, i) => `m${i}`);
  assert.equal(previewStockPolicyBatch(atLimit, ['w1']).overLimit, false);
  const over = Array.from({ length: MAX_BATCH_CELLS + 1 }, (_, i) => `m${i}`);
  assert.equal(previewStockPolicyBatch(over, ['w1']).overLimit, true);
  assert.ok(BACKEND_DTO.includes('pub const MAX_BATCH_CELLS: usize = 2000;'), '后端上限必须也是 2000');
});

test('批量提交载荷：发出的是去重后的 id 列表（同格两次会撞唯一键）', () => {
  const payload = buildStockPolicyBatchPayload({
    ...emptyStockPolicyBatchForm(),
    material_ids: ['m1', 'm1'],
    warehouse_ids: ['w1'],
    min_qty: ' 120 ',
    max_qty: '',
    remark: '  ',
  });
  assert.deepEqual(payload.material_ids, ['m1']);
  assert.deepEqual(payload.warehouse_ids, ['w1']);
  assert.equal(payload.min_qty, '120');
  assert.equal(payload.max_qty, null);
  assert.equal(payload.remark, null);
});

// ==================== ⑤ 批量提交前校验 ====================

test('批量校验：没选物料 / 没选仓库 分别拦（不让他白填四列）', () => {
  const noMaterial = validateStockPolicyBatch({ ...emptyStockPolicyBatchForm(), warehouse_ids: ['w1'] });
  assert.ok(noMaterial?.includes('物料'), noMaterial ?? '');

  const noWarehouse = validateStockPolicyBatch({ ...emptyStockPolicyBatchForm(), material_ids: ['m1'] });
  assert.ok(noWarehouse?.includes('仓库'), noWarehouse ?? '');
});

test('批量校验：超上限给出「减少物料 / 改用导入」的替代动作', () => {
  const materials = Array.from({ length: MAX_BATCH_CELLS + 1 }, (_, i) => `m${i}`);
  const error = validateStockPolicyBatch({
    ...emptyStockPolicyBatchForm(),
    material_ids: materials,
    warehouse_ids: ['w1'],
    min_qty: '10',
  });
  assert.ok(error?.includes('2000'), error ?? '');
  assert.ok(error?.includes('导入'), error ?? '');
});

test('批量校验：四列非法沿用单格校验文案；无 save 权限直接拒', () => {
  const badLevels = validateStockPolicyBatch({
    ...emptyStockPolicyBatchForm(),
    material_ids: ['m1'],
    warehouse_ids: ['w1'],
    min_qty: '100',
    max_qty: '100',
  });
  assert.ok(badLevels?.includes('严格大于'), badLevels ?? '');

  const noPerm = validateStockPolicyBatch(
    { ...emptyStockPolicyBatchForm(), material_ids: ['m1'], warehouse_ids: ['w1'], min_qty: '1' },
    false,
  );
  assert.ok(noPerm?.includes(PERM_STOCK_POLICY_SAVE), noPerm ?? '');
});

test('批量校验：合法入参返回 null', () => {
  assert.equal(
    validateStockPolicyBatch({
      ...emptyStockPolicyBatchForm(),
      material_ids: ['m1', 'm2'],
      warehouse_ids: ['w1'],
      min_qty: '100',
      max_qty: '500',
    }),
    null,
  );
});

test('批量预览文案：格数 / 去重提示 / 覆盖语义 都要说出来', () => {
  const text = batchPreviewText(previewStockPolicyBatch(['m1', 'm1', 'm2'], ['w1', 'w2']));
  assert.ok(text.includes('4 格'), text);
  assert.ok(text.includes('2 个物料'), text);
  assert.ok(text.includes('重复物料 1'), text);
  assert.ok(text.includes('覆盖'), text);

  const over = batchPreviewText(previewStockPolicyBatch(Array.from({ length: 2001 }, (_, i) => `m${i}`), ['w1']));
  assert.ok(over.includes('超过单次上限'), over);
  assert.ok(over.includes('导入'), over);
});

// ==================== ⑥ 格式化（Decimal 字符串 / null 的展示口径） ====================

test('数量格式化：null → —（未设最高水位不能显示成 0）', () => {
  assert.equal(formatQty(null), '—');
  assert.equal(formatQty(undefined), '—');
  assert.equal(formatQty(''), '—');
});

test('数量格式化：去尾零 + 千分位', () => {
  assert.equal(formatQty('120.0000'), '120');
  assert.equal(formatQty('1234.5000'), '1,234.5');
  assert.equal(formatQty(0), '0');
  assert.equal(formatQty('1000000'), '1,000,000');
});

test('数量文本：回填不丢精度、不补零', () => {
  assert.equal(qtyText('100.0000'), '100');
  assert.equal(qtyText('100.5000'), '100.5');
  assert.equal(qtyText(null), '');
  assert.equal(qtyText(0), '0');
});

test('最低水位展示：0 要说成「0（不预警）」（0 与「没设」必须能区分）', () => {
  assert.equal(minQtyText(0), '0（不预警）');
  assert.equal(minQtyText('0.0000'), '0（不预警）');
  assert.equal(minQtyText('100.0000'), '100');
});

test('状态文案 / 主题 / 说明三处一致', () => {
  assert.equal(statusLabel(1), '启用');
  assert.equal(statusLabel(0), '停用');
  assert.equal(statusTheme(1), 'success');
  assert.equal(statusTheme(0), 'default');
  assert.ok(statusHint(0).includes('物料级'), statusHint(0));
  assert.ok(statusHint(1).includes('本行水位'), statusHint(1));
});

test('主数据被删时给一句人话（不是空白）', () => {
  assert.equal(materialText({ material_code: null, material_name: null }), '物料已删除');
  assert.equal(warehouseText({ warehouse_code: null, warehouse_name: null }), '仓库已删除');
  assert.equal(materialText({ material_code: 'M1', material_name: '螺丝' }), 'M1 螺丝');
  assert.equal(materialText({ material_code: 'M1', material_name: null }), 'M1');
});

test('空态文案分「还没设过」与「筛选没匹配」', () => {
  assert.ok(emptyText(false).includes('还没有任何水位设置'), emptyText(false));
  assert.ok(emptyText(true).includes('清空筛选'), emptyText(true));
});

// ==================== ⑦ 源码即契约 ====================

test('API：路径写全 /api，四个面都在（列表 / 单格 / 批量 / 删除）', () => {
  assert.ok(API.includes("const BASE = '/api/business/erp'"), '漏 /api 会命中 SPA fallback 返回 HTML');
  assert.ok(API.includes('${BASE}/stock-policy/page'));
  assert.ok(API.includes('put<StockPolicyRow>(`${BASE}/stock-policy`'), '单格是 PUT（upsert，不是 POST）');
  assert.ok(API.includes('put<StockPolicyBatchResult>(`${BASE}/stock-policy/batch`'), '批量是 PUT /batch');
  assert.ok(API.includes('del<void>(`${BASE}/stock-policy/${id}`)'));
});

test('页面：导入走通用导入向导（biz-type 常量），不自己解析 xlsx', () => {
  const pageCode = withoutComments(PAGE);
  assert.ok(TPL.includes('ImportEntryButton'), '模板里必须有导入入口组件');
  assert.ok(TPL.includes(':biz-type="STOCK_POLICY_IMPORT_BIZ_TYPE"'));
  assert.ok(PAGE.includes("from '@/components/ImportEntryButton.vue'"));
  assert.ok(!pageCode.includes('xlsx'), '页面不许自己解析 Excel（那是 impexp 的活）');
  assert.ok(!pageCode.includes('multipart/form-data'), '页面不许自己上传文件');
});

test('页面：单格弹窗 + 批量弹窗都在，且批量弹窗显示格数预览', () => {
  assert.ok(TPL.includes('data-testid="stock-policy-cell-dialog"'));
  assert.ok(TPL.includes('data-testid="stock-policy-batch-dialog"'));
  assert.ok(TPL.includes('stock-policy-batch-preview'));
  assert.ok(TPL.includes('batchPreviewText(batchPreview)'), '预览文案必须来自纯逻辑（可单测）');
});

test('页面：编辑时物料 / 仓库不可改（键不可换）', () => {
  assert.ok(TPL.includes(':disabled="isEdit"'), '物料 / 仓库是键：编辑时必须禁用');
});

test('页面：按码决定列表可用性与按钮可点性（不让人点了才吃 403）', () => {
  assert.ok(PAGE.includes('PERM_STOCK_POLICY_LIST'));
  assert.ok(PAGE.includes('PERM_STOCK_POLICY_SAVE'));
  assert.ok(PAGE.includes('PERM_STOCK_POLICY_DELETE'));
  assert.ok(TPL.includes('stock-policy-no-perm'), '没有列表码要有说明条');
  assert.ok(TPL.includes(':disabled="!canSave"'), '没有 save 码要把按钮置灰');
});

test('导入 biz-type 与后端定义逐字一致（漂了 = 下载模板 404 60011）', () => {
  assert.equal(STOCK_POLICY_IMPORT_BIZ_TYPE, 'business.erp.stock-policy');
  assert.ok(LOGIC.includes(`STOCK_POLICY_IMPORT_BIZ_TYPE = 'business.erp.stock-policy'`), '逻辑层的常量必须是这一个值');
  assert.ok(
    BACKEND_IMPORT.includes('pub const STOCK_POLICY_BIZ_TYPE: &str = "business.erp.stock-policy";'),
    '后端常量必须与页面常量一致',
  );
});

test('路由：path 与迁移 sys_menu.path 一致，且 meta.permission 用页面显形码', () => {
  assert.equal(PERM_STOCK_POLICY_PAGE, 'business:erp:stock-policy:page');
  assert.ok(ROUTER.includes("path: 'business/erp/stock-policy'"), '路由 path 与菜单 path 必须一致（否则点菜单 404）');
  assert.ok(
    ROUTER.includes("meta: { title: '安全库存', permission: 'business:erp:stock-policy:page' }"),
    '路由必须判页面显形码（不是接口码）',
  );
});

test('迁移 V0431：页面行形状（页面码 / path / component / sort 47 / icon setting / 挂仓库管理）', () => {
  assert.ok(MIG_UP.includes('20000000-0000-0000-0000-000000004310'), '页面行固定 UUID');
  assert.ok(MIG_UP.includes('business:erp:stock-policy:page'));
  assert.ok(MIG_UP.includes("'/business/erp/stock-policy'"));
  assert.ok(MIG_UP.includes("'business/erp/stock-policy/index'"), 'component 与前端 views 目录逐字一致');
  assert.ok(MIG_UP.includes('20000000-0000-0000-0000-000000000e01'), '必须挂「仓库管理」目录（祖先闭包）');
  assert.ok(/icon = 'setting'|'setting'/.test(MIG_UP), 'icon 必须是 TDesign 图标库真实名');
  assert.ok(MIG_UP.includes('sort_order = 47'), '同父 sort 必须唯一（约定 12）');
  // 四个接口码一个都不能在本迁移里被重新种成按钮行（上游钉死「各 1 个 menu_type=3 节点」）
  for (const code of [
    'business:erp:stock-policy:list',
    'business:erp:stock-policy:save',
    'business:erp:stock-policy:delete',
    'business:erp:stock-policy:check',
  ]) {
    assert.ok(
      !MIG_UP.includes(`'${code}', 3,`),
      `${code} 在本迁移里只应被引用（授权 / 自检），不得被重新 INSERT 成按钮行`,
    );
  }
  assert.ok(!/\bINSERT INTO sys_menu\b[^;]*'business:erp:stock-policy:(list|save|delete|check)',\s*3,/.test(MIG_UP), '接口码不得被本迁移种成按钮行');
});

test('迁移 V0431：幂等（ON CONFLICT DO NOTHING + 授权 NOT EXISTS）与回滚只删自己的行', () => {
  assert.ok(MIG_UP.includes('ON CONFLICT (id) DO NOTHING'));
  assert.ok(MIG_UP.includes('NOT EXISTS'));
  assert.ok(MIG_DOWN.includes('20000000-0000-0000-0000-000000004310'), 'down 必须删页面行');
  assert.ok(!MIG_DOWN.includes('business:erp:stock-policy:list'), 'down 不得动四个接口码的授权');
});

test('权限码常量与后端 / 迁移逐字一致（四个接口码 + 一个页面码）', () => {
  assert.equal(PERM_STOCK_POLICY_LIST, 'business:erp:stock-policy:list');
  assert.equal(PERM_STOCK_POLICY_SAVE, 'business:erp:stock-policy:save');
  assert.equal(PERM_STOCK_POLICY_DELETE, 'business:erp:stock-policy:delete');
  for (const code of [PERM_STOCK_POLICY_LIST, PERM_STOCK_POLICY_SAVE, PERM_STOCK_POLICY_DELETE]) {
    assert.ok(BACKEND_DTO.includes(code) || MIG_UP.includes(code), `${code} 必须在后端或迁移里出现过`);
  }
});
