/**
 * CODE-05 单测：供应商 + 客户表单「生成编码」按钮（交互方式 B）。
 *
 * # 这次要钉死的四件事
 *
 * 1. **编码不再是前端必填**：留空 ⇒ 后端自动取号。前端还在拦「编码必填」的话，
 *    卡片验收 ④（不点生成直接保存）会被前端自己挡住 —— 这是 CODE-02 交接里点名的坑。
 * 2. **留空发 `null`**（不是 `''`）：语义更准，后端 `resolve_manual_code` 两种都当「没填」。
 * 3. **按钮真的在**：`.vue` 里要有「生成编码」按钮 + `:disabled="isEdit"` + 调预览接口。
 *    这条只能读源码钉（`vue-tsc` 抓得到类型错，抓不到「按钮被删了」「编辑态忘了禁用」）。
 * 4. **API 路径写全** `/api`：漏前缀会命中 SPA fallback 返回 HTML（仓内高频坑）。
 *
 * 运行：`pnpm test:unit`
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  buildCustomerCreatePayload,
  emptyCustomerForm,
  validateCustomerForm,
} from '../../src/views/business/erp/customer/customer-query';
import {
  buildSupplierCreatePayload,
  emptySupplierForm,
  validateSupplierForm,
} from '../../src/views/business/erp/supplier/supplier-query';

const HERE = dirname(fileURLToPath(import.meta.url));
const SRC = resolve(HERE, '../../src');

function read(relative: string): string {
  return readFileSync(join(SRC, relative), 'utf8');
}

// ==================== ① 校验：编码可留空 ====================

test('validateSupplierForm：编码留空不再拦（后端自动取号），超长仍拦', () => {
  const blank = validateSupplierForm(emptySupplierForm(), false);
  assert.equal(blank.supplier_code, undefined, '留空不该报「供应商编码必填」');
  assert.equal(blank.supplier_name, '供应商名称必填', '名称仍然必填');

  const long = validateSupplierForm({ ...emptySupplierForm(), supplier_code: 'x'.repeat(41), supplier_name: 'n' }, false);
  assert.equal(long.supplier_code, '供应商编码最多 40 个字符', '超长仍要拦');
});

test('validateCustomerForm：编码留空不再拦（后端自动取号），超长仍拦', () => {
  const blank = validateCustomerForm(emptyCustomerForm(), false);
  assert.equal(blank.code, undefined, '留空不该报「客户编码必填」');
  assert.equal(blank.name, '客户名称必填', '名称仍然必填');

  const long = validateCustomerForm({ ...emptyCustomerForm(), code: 'c'.repeat(41), name: 'n' }, false);
  assert.equal(long.code, '客户编码最多 40 个字符', '超长仍要拦');
});

// ==================== ② 请求体：留空发 null，填了原样 trim ====================

test('新建请求体：编码留空发 null（交给后端取号），填了只 trim', () => {
  const supplierBlank = buildSupplierCreatePayload({ ...emptySupplierForm(), supplier_name: 'n' });
  assert.equal(supplierBlank.supplier_code, null, '供应商：留空必须是 null');
  const supplierManual = buildSupplierCreatePayload({
    ...emptySupplierForm(),
    supplier_code: '  GYS-001  ',
    supplier_name: 'n',
  });
  assert.equal(supplierManual.supplier_code, 'GYS-001', '供应商：手工编码原样保留（只 trim）');

  const customerBlank = buildCustomerCreatePayload({ ...emptyCustomerForm(), name: 'n' });
  assert.equal(customerBlank.code, null, '客户：留空必须是 null');
  const customerManual = buildCustomerCreatePayload({
    ...emptyCustomerForm(),
    code: '  KH-001  ',
    name: 'n',
  });
  assert.equal(customerManual.code, 'KH-001', '客户：手工编码原样保留（只 trim）');
});

// ==================== ③ API 层：预览接口 + 路径写全 ====================

test('API：next-code 预览接口存在且路径写全 /api', () => {
  const api = read('api/business/erp/index.ts');
  assert.match(api, /export function fetchSupplierNextCode\(\): Promise<NextCodePreview>/, '缺 fetchSupplierNextCode');
  assert.match(api, /export function fetchCustomerNextCode\(\): Promise<NextCodePreview>/, '缺 fetchCustomerNextCode');
  // `${BASE}` 已含 /api；这里断言拼出来的相对路径片段，防止有人手写成别的前缀
  assert.match(api, /`\$\{BASE\}\/supplier\/next-code`/, '供应商预览路径写成 ${BASE}/supplier/next-code');
  assert.match(api, /`\$\{BASE\}\/customer\/next-code`/, '客户预览路径写成 ${BASE}/customer/next-code');
});

// ==================== ④ 页面：按钮在、编辑态禁用、保存后回填 ====================

test('供应商页：编码栏旁有「生成编码」按钮，编辑态禁用，保存后回填编码', () => {
  const vue = read('views/business/erp/supplier/index.vue');
  assert.ok(vue.includes('生成编码'), '要有「生成编码」按钮文案');
  assert.ok(vue.includes('fetchSupplierNextCode'), '要点预览接口');
  assert.ok(vue.includes('data-testid="sup-gen-code"'), '按钮要带 testid（e2e / 自检用）');
  assert.match(
    vue,
    /<t-button[\s\S]*?:disabled="isEdit"[\s\S]*?data-testid="sup-gen-code"/,
    '按钮必须 :disabled="isEdit"（编辑态禁用）',
  );
  assert.ok(vue.includes('不点「生成编码」则保存时自动生成'), '要有 help 提示文案');
  assert.ok(vue.includes('form.supplier_code = created.supplier_code'), '保存后要把后端取的号回填表单');
});

test('客户页：编码栏旁有「生成编码」按钮，编辑态禁用，保存后回填编码', () => {
  const vue = read('views/business/erp/customer/index.vue');
  assert.ok(vue.includes('生成编码'), '要有「生成编码」按钮文案');
  assert.ok(vue.includes('fetchCustomerNextCode'), '要点预览接口');
  assert.ok(vue.includes('data-testid="cus-gen-code"'), '按钮要带 testid');
  assert.match(
    vue,
    /<t-button[\s\S]*?:disabled="isEdit"[\s\S]*?data-testid="cus-gen-code"/,
    '按钮必须 :disabled="isEdit"（编辑态禁用）',
  );
  assert.ok(vue.includes('不点「生成编码」则保存时自动生成'), '要有 help 提示文案');
  assert.ok(vue.includes('form.code = created.code'), '保存后要把后端取的号回填表单');
});

test('两页的编码输入框仍然是「编辑态只读」', () => {
  const supplier = read('views/business/erp/supplier/index.vue');
  const customer = read('views/business/erp/customer/index.vue');
  // CODE-08 起，「编辑态只读」不再是裸 `:disabled="isEdit"`：编码框还要受
  // 系统参数 `erp.master_data.allow_manual_code` 控制（关闭时新建也不可填），
  // 两条合并在 `manualCodeDisabled(isEdit, allowManualCode)` 里。
  // 本用例护的语义不变：编辑态的编码框必须禁用（该函数对 `isEdit = true` 恒返回 true，
  // 见 code08-manual-code-switch.test.ts 的纯逻辑用例）。
  assert.match(
    supplier,
    /v-model="form\.supplier_code"\s*\n?\s*:disabled="manualCodeDisabled\(isEdit, allowManualCode\)"/,
    '供应商编码框必须走 manualCodeDisabled(isEdit, allowManualCode)（编辑态仍只读）',
  );
  assert.match(
    customer,
    /v-model="form\.code"\s*\n?\s*:disabled="manualCodeDisabled\(isEdit, allowManualCode\)"/,
    '客户编码框必须走 manualCodeDisabled(isEdit, allowManualCode)（编辑态仍只读）',
  );
});
