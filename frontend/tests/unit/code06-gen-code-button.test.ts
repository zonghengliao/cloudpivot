/**
 * CODE-06 单测：物料 + 仓库表单「生成编码」按钮（交互方式 B，同 CODE-05 模式）。
 *
 * # 每个表单四条用例，钉死四件事
 *
 * 1. **编码不再是前端必填**：留空 ⇒ 后端自动取号（`MAT-0001` / `WH-01`）。
 *    前端还在拦「编码必填」的话，卡片验收 ④（不点生成直接保存）会被前端自己挡住。
 * 2. **留空发 `null`**（不是 `''`）：语义更准，后端 `resolve_manual_code` 两种都当「没填」。
 * 3. **API 层有预览接口且路径写全** `/api`：漏前缀会命中 SPA fallback 返回 HTML（仓内高频坑）。
 * 4. **按钮真的在**：`.vue` 里要有「生成编码」按钮 + `:disabled="isEdit"` + 调预览接口 +
 *    保存后回填后端真正用的编码。这条只能读源码钉（`vue-tsc` 抓得到类型错，
 *    抓不到「按钮被删了」「编辑态忘了禁用」）。
 *
 * 仓库表单的实际控件在 `warehouse/components/WarehouseFormDialog.vue`（card 写的
 * `warehouse/index.vue` 只是挂这个弹窗），所以断言落在 dialog 上，并额外钉一条
 * 「index.vue 仍然引用了这个弹窗」防止有人把抽屉挪走后测试假绿。
 *
 * 运行：`pnpm test:unit`
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  buildMaterialCreatePayload,
  emptyMaterialForm,
  validateMaterialForm,
} from '../../src/views/business/erp/material/material-query';
import {
  buildWarehouseCreatePayload,
  emptyWarehouseForm,
  validateWarehouseForm,
} from '../../src/views/business/erp/warehouse/warehouse-tree';

const HERE = dirname(fileURLToPath(import.meta.url));
const SRC = resolve(HERE, '../../src');

function read(relative: string): string {
  return readFileSync(join(SRC, relative), 'utf8');
}

// ==================== 物料 ① 校验：编码可留空 ====================

test('物料·①validateMaterialForm：编码留空不拦（后端自动取号 MAT-0001），超长仍拦', () => {
  const blank = validateMaterialForm(emptyMaterialForm(), false);
  assert.equal(blank.material_code, undefined, '留空不该报「物料编码必填」');
  assert.equal(blank.material_name, '物料名称必填', '名称仍然必填');
  assert.ok(blank.unit_id, '计量单位仍然必填');

  const long = validateMaterialForm(
    { ...emptyMaterialForm(), material_code: 'x'.repeat(41), material_name: 'n', unit_id: 'u' },
    false,
  );
  assert.equal(long.material_code, '物料编码最多 40 个字符', '超长仍要拦');
});

// ==================== 物料 ② 请求体：留空发 null ====================

test('物料·②新建请求体：编码留空发 null（交给后端取号），填了只 trim', () => {
  const blank = buildMaterialCreatePayload({
    ...emptyMaterialForm(),
    material_name: 'n',
    unit_id: 'u',
  });
  assert.equal(blank.material_code, null, '留空必须是 null');

  const manual = buildMaterialCreatePayload({
    ...emptyMaterialForm(),
    material_code: '  YB-001-02  ',
    material_name: 'n',
    unit_id: 'u',
  });
  assert.equal(manual.material_code, 'YB-001-02', '手工编码原样保留（只 trim，按分类自编号也要能用）');
});

// ==================== 物料 ③ API 层：预览接口 + 路径写全 ====================

test('物料·③API：fetchMaterialNextCode 存在且路径写全 /api', () => {
  const api = read('api/business/erp/index.ts');
  assert.match(
    api,
    /export function fetchMaterialNextCode\(\): Promise<NextCodePreview>/,
    '缺 fetchMaterialNextCode',
  );
  // `${BASE}` 已含 /api；这里断言拼出来的相对路径片段，防止有人手写成别的前缀
  assert.match(api, /`\$\{BASE\}\/material\/next-code`/, '物料预览路径写成 ${BASE}/material/next-code');
});

// ==================== 物料 ④ 页面：按钮在、编辑态禁用、保存后回填 ====================

test('物料·④页面：编码栏旁有「生成编码」按钮，编辑态禁用，保存后回填编码', () => {
  const vue = read('views/business/erp/material/index.vue');
  assert.ok(vue.includes('生成编码'), '要有「生成编码」按钮文案');
  assert.ok(vue.includes('fetchMaterialNextCode'), '要点预览接口');
  assert.ok(vue.includes('data-testid="mat-gen-code"'), '按钮要带 testid（e2e / 自检用）');
  assert.match(
    vue,
    /<t-button[\s\S]*?:disabled="isEdit"[\s\S]*?data-testid="mat-gen-code"/,
    '按钮必须 :disabled="isEdit"（编辑态禁用）',
  );
  assert.ok(vue.includes('不点「生成编码」则保存时自动生成'), '要有 help 提示文案');
  assert.ok(
    vue.includes('form.material_code = created.material_code'),
    '保存后要把后端取的号回填表单',
  );
  assert.match(
    vue,
    /v-model="form\.material_code"[\s\S]*?:disabled="isEdit"/,
    '编码输入框必须保持 :disabled="isEdit"',
  );
});

// ==================== 仓库 ① 校验：编码可留空 ====================

test('仓库·①validateWarehouseForm：编码留空不拦（后端自动取号 WH-01，2 位），超长仍拦', () => {
  const blank = validateWarehouseForm(emptyWarehouseForm(), null);
  assert.equal(blank.warehouse_code, undefined, '留空不该报「请填仓库编码」');
  assert.equal(blank.warehouse_name, '请填仓库名称', '名称仍然必填');

  const long = validateWarehouseForm(
    { ...emptyWarehouseForm(), warehouse_code: 'x'.repeat(41), warehouse_name: 'n' },
    null,
  );
  assert.equal(long.warehouse_code, '编码最多 40 个字符', '超长仍要拦');
});

// ==================== 仓库 ② 请求体：留空发 null ====================

test('仓库·②新建请求体：编码留空发 null（交给后端取号），填了只 trim', () => {
  const blank = buildWarehouseCreatePayload(
    { ...emptyWarehouseForm(), warehouse_name: 'n' },
    null,
  );
  assert.equal(blank.warehouse_code, null, '留空必须是 null');
  assert.equal(blank.level, 1, '顶级仓库 level 仍是 1（取号不影响层级推导）');

  const manual = buildWarehouseCreatePayload(
    { ...emptyWarehouseForm(), warehouse_code: '  CK-001  ', warehouse_name: 'n' },
    null,
  );
  assert.equal(manual.warehouse_code, 'CK-001', '手工编码原样保留（企业自有编码体系要能对接）');
});

// ==================== 仓库 ③ API 层：预览接口 + 路径写全 ====================

test('仓库·③API：fetchWarehouseNextCode 存在且路径写全 /api', () => {
  const api = read('api/business/erp/index.ts');
  assert.match(
    api,
    /export function fetchWarehouseNextCode\(\): Promise<NextCodePreview>/,
    '缺 fetchWarehouseNextCode',
  );
  assert.match(
    api,
    /`\$\{BASE\}\/warehouse\/next-code`/,
    '仓库预览路径写成 ${BASE}/warehouse/next-code',
  );
});

// ==================== 仓库 ④ 页面：按钮在、编辑态禁用、保存后回填 ====================

test('仓库·④弹窗：编码栏旁有「生成编码」按钮，编辑态禁用，保存后回填编码', () => {
  const dialog = read('views/business/erp/warehouse/components/WarehouseFormDialog.vue');
  assert.ok(dialog.includes('生成编码'), '要有「生成编码」按钮文案');
  assert.ok(dialog.includes('fetchWarehouseNextCode'), '要点预览接口');
  assert.ok(dialog.includes('data-testid="wh-gen-code"'), '按钮要带 testid');
  assert.match(
    dialog,
    /<t-button[\s\S]*?:disabled="isEdit"[\s\S]*?data-testid="wh-gen-code"/,
    '按钮必须 :disabled="isEdit"（编辑态禁用）',
  );
  assert.ok(dialog.includes('不点「生成编码」则保存时自动生成'), '要有 help 提示文案');
  assert.ok(
    dialog.includes('form.warehouse_code = saved.warehouse_code'),
    '保存后要把后端取的号回填表单',
  );
  assert.match(
    dialog,
    /v-model="form\.warehouse_code"[\s\S]*?:disabled="isEdit"/,
    '编码输入框必须保持 :disabled="isEdit"',
  );

  // 防假绿：页面必须真的还挂着这个弹窗（否则上面断言的是「死代码」）
  const page = read('views/business/erp/warehouse/index.vue');
  assert.ok(
    page.includes("import WarehouseFormDialog from './components/WarehouseFormDialog.vue'"),
    'warehouse/index.vue 必须引入 WarehouseFormDialog',
  );
  assert.match(page, /<WarehouseFormDialog[\s\S]*?v-model:visible="warehouseDialogVisible"/, '弹窗要真挂上');
});
