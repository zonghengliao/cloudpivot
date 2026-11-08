/**
 * CODE-07 单测：资产分类 + 费用类型表单「生成编码」按钮（交互方式 B，同 CODE-05/06 模式）。
 *
 * # 每个表单四条用例，钉死四件事
 *
 * 1. **编码不再被前端拦成必填**：留空 ⇒ 后端自动取号（`AC-01` / `ET-01`，2 位纯流水、
 *    不计日期、永不重置）。前端还拦「编码必填」的话，卡片验收 ④（不点生成直接保存）
 *    会被前端自己挡住。**同时钉**「取号器自己产出的 `AC-01` / `ET-01` 必须能通过校验」
 *    —— 旧正则 `^[A-Z][A-Z0-9_]*$` 不接受连字符，点完「生成编码」保存不过去。
 * 2. **留空发 `null`**（不是 `''`）：与 CODE-05/06 同一口径，后端 `resolve_manual_code`
 *    两种都当「没填」，但 `null` 一眼看出意图；填了原样保留（转大写，企业自有码
 *    `CAT-BUILDING` / `ADMIN-FEE` 要能对接）。
 * 3. **API 层有预览接口且路径写全** `/api`：漏前缀会命中 SPA fallback 返回 HTML
 *    （仓内高频坑）。资产分类的接口在 `api/business/erp/asset.ts`（不是 index.ts），
 *    费用类型在 `api/business/erp/expense.ts` —— 与供应商/物料那几个不同文件，别找错。
 * 4. **按钮真的在**：`.vue` 里要有「生成编码」按钮 + 编辑态禁用 + 调预览接口 +
 *    保存后回填后端真正用的编码。这条只能读源码钉（`vue-tsc` 抓得到类型错，抓不到
 *    「按钮被删了」「编辑态忘了禁用」）。
 *
 * 运行：`pnpm test:unit`
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  emptyCategoryForm,
  validateCategoryForm,
} from '../../src/views/business/erp/asset/asset-logic';
import {
  emptyTypeForm,
  toTypePayload,
  validateTypeForm,
} from '../../src/views/business/erp/expense/expense-logic';
import {
  MASTER_CODE_MAX_LEN,
  masterCodeError,
  toMasterCodeInput,
} from '../../src/views/business/erp/shared/master-code';

const HERE = dirname(fileURLToPath(import.meta.url));
const SRC = resolve(HERE, '../../src');

function read(relative: string): string {
  return readFileSync(join(SRC, relative), 'utf8');
}

/** 一个「除了编码以外都填好」的分类表单（隔离校验目标）。 */
function filledCategoryForm() {
  return {
    ...emptyCategoryForm(),
    category_name: '房屋建筑物',
    useful_life_months: 240,
    asset_account_id: 'acc-asset',
    accum_account_id: 'acc-accum',
    expense_account_id: 'acc-expense',
  };
}

/** 一个「除了编码以外都填好」的费用类型表单（隔离校验目标）。 */
function filledTypeForm() {
  return { ...emptyTypeForm(), type_name: '差旅费', account_id: 'acc-travel' };
}

// ==================== 资产分类 ① 校验：编码可留空，AC-01 形态放行 ====================

test('资产分类·①validateCategoryForm：编码留空不拦（后端取号 AC-01），取号形态与超长都按字符集判', () => {
  const blank = validateCategoryForm(filledCategoryForm());
  assert.equal(blank.category_code, undefined, '留空不该报「分类编码必填」');
  assert.equal(blank.category_name, undefined, '其它字段填好时不该有别的错');

  // 名称仍必填（别为了放行编码顺手把名称校验也删了）
  assert.equal(validateCategoryForm(emptyCategoryForm()).category_name, '分类名称必填', '名称仍然必填');

  // 取号器产物 `AC-01` / 第 100 个起的 `AC-100`、企业自有码 `CAT-BUILDING` 都必须放行
  for (const code of ['AC-01', 'AC-100', 'CAT-BUILDING', 'ac-01', '  AC-02  ']) {
    assert.equal(
      validateCategoryForm({ ...filledCategoryForm(), category_code: code }).category_code,
      undefined,
      `编码「${code}」应通过（连字符是取号器自身产物，不能拦）`,
    );
  }

  // 超长 / 空格仍拦，文案与后端逐字对齐
  assert.equal(
    validateCategoryForm({ ...filledCategoryForm(), category_code: 'x'.repeat(41) }).category_code,
    `编码最长 ${MASTER_CODE_MAX_LEN} 个字符（当前 41 个）`,
    '超长仍要拦',
  );
  assert.match(
    validateCategoryForm({ ...filledCategoryForm(), category_code: 'CAT BUILDING' }).category_code ?? '',
    /编码里不能出现「 」/,
    '空格仍要拦',
  );
});

// ==================== 资产分类 ② 请求体：留空发 null，填了大写 trim ====================

test('资产分类·②请求体：编码留空发 null（交给后端取号），填了转大写只 trim', () => {
  // 页面 toPayload 直接调这套 helper（源码钉在 ④），这里钉语义本身
  assert.equal(toMasterCodeInput(''), null, '空串 ⇒ null');
  assert.equal(toMasterCodeInput('   '), null, '纯空白 ⇒ null');
  assert.equal(toMasterCodeInput(undefined), null, 'undefined ⇒ null');
  assert.equal(toMasterCodeInput('  ac-01 '), 'AC-01', '手工/预览编码原样保留，只 trim + 转大写');
  assert.equal(toMasterCodeInput('cat-building'), 'CAT-BUILDING', '企业自有编码可对接');
  assert.equal(masterCodeError(''), null, '留空 = 通过（交给后端取号）');
});

// ==================== 资产分类 ③ API 层：预览接口 + 路径写全 ====================

test('资产分类·③API：fetchAssetCategoryNextCode 存在、路径写全 /api、契约复用 NextCodePreview', () => {
  const api = read('api/business/erp/asset.ts');
  assert.match(api, /^const BASE = '\/api\/business\/erp';$/m, 'BASE 必须含 /api 前缀');
  assert.match(
    api,
    /export function fetchAssetCategoryNextCode\(\): Promise<NextCodePreview>/,
    '缺 fetchAssetCategoryNextCode',
  );
  assert.match(
    api,
    /`\$\{BASE\}\/asset-category\/next-code`/,
    '资产分类预览路径写成 ${BASE}/asset-category/next-code',
  );
  assert.match(
    api,
    /import type \{ NextCodePreview \} from '\.\/index'/,
    '响应体契约复用 index.ts 的 NextCodePreview（别各写一份）',
  );
  // 入参类型必须允许 null（留空 = 后端取号），否则「不点生成直接保存」类型不过
  assert.match(api, /category_code\?: string \| null;/, 'AssetCategoryPayload.category_code 要允许 null');
});

// ==================== 资产分类 ④ 页面：按钮在、编辑态禁用、保存后回填 ====================

test('资产分类·④页面：编码栏旁有「生成编码」按钮，编辑态禁用，保存后回填后端编码', () => {
  const vue = read('views/business/erp/asset-category/index.vue');
  assert.ok(vue.includes('生成编码'), '要有「生成编码」按钮文案');
  assert.ok(vue.includes('fetchAssetCategoryNextCode'), '要点预览接口');
  assert.ok(vue.includes('data-testid="category-gen-code"'), '按钮要带 testid（e2e / 自检用）');
  assert.match(
    vue,
    /<t-button[\s\S]*?:disabled="!!editingId"[\s\S]*?data-testid="category-gen-code"/,
    '按钮必须编辑态禁用',
  );
  assert.ok(vue.includes('handleGenCode'), '要有点击处理函数');
  assert.match(vue, /form\.category_code = preview\.code/, '预览号要填进编码框（可手改）');
  assert.match(
    vue,
    /v-model="form\.category_code"[\s\S]*?:disabled="!!editingId"/,
    '编码输入框必须编辑态只读',
  );
  assert.match(
    vue,
    /category_code: toMasterCodeInput\(form\.category_code\)/,
    '提交时必须走 toMasterCodeInput（留空 ⇒ null）',
  );
  assert.ok(
    vue.includes('form.category_code = created.category_code'),
    '保存后要把后端真正用的编码回填表单',
  );
  assert.ok(vue.includes('不点「生成编码」则保存时自动生成'), '要有 help 提示文案');
});

// ==================== 费用类型 ① 校验：编码可留空，ET-01 形态放行 ====================

test('费用类型·①validateTypeForm：编码留空不拦（后端取号 ET-01），取号形态与超长都按字符集判', () => {
  assert.equal(validateTypeForm(filledTypeForm()), null, '留空不该报「费用类型编码必填」');
  assert.equal(
    validateTypeForm(emptyTypeForm()),
    '费用类型名称必填',
    '编码不拦后，第一个错应是名称（别把编码校验删成整条短路）',
  );

  for (const code of ['ET-01', 'ET-100', 'ADMIN-FEE', 'travel', '  ET-02  ']) {
    assert.equal(
      validateTypeForm({ ...filledTypeForm(), type_code: code }),
      null,
      `编码「${code}」应通过（连字符是取号器自身产物，不能拦）`,
    );
  }

  assert.equal(
    validateTypeForm({ ...filledTypeForm(), type_code: 'x'.repeat(41) }),
    `编码最长 ${MASTER_CODE_MAX_LEN} 个字符（当前 41 个）`,
    '超长仍要拦',
  );
  assert.match(
    validateTypeForm({ ...filledTypeForm(), type_code: 'ET 01' }) ?? '',
    /编码里不能出现「 」/,
    '空格仍要拦',
  );
});

// ==================== 费用类型 ② 请求体：留空发 null ====================

test('费用类型·②toTypePayload：编码留空发 null（交给后端取号），填了转大写只 trim', () => {
  assert.equal(toTypePayload(filledTypeForm()).type_code, null, '留空必须是 null');
  assert.equal(
    toTypePayload({ ...filledTypeForm(), type_code: '   ' }).type_code,
    null,
    '纯空白也是 null',
  );
  assert.equal(
    toTypePayload({ ...filledTypeForm(), type_code: '  et-01  ' }).type_code,
    'ET-01',
    '预览号原样保留（只 trim + 转大写）',
  );
  assert.equal(
    toTypePayload({ ...filledTypeForm(), type_code: 'admin-fee' }).type_code,
    'ADMIN-FEE',
    '企业自有编码可对接',
  );
});

// ==================== 费用类型 ③ API 层：预览接口 + 路径写全 ====================

test('费用类型·③API：fetchExpenseTypeNextCode 存在、路径写全 /api、契约复用 NextCodePreview', () => {
  const api = read('api/business/erp/expense.ts');
  assert.match(api, /^const BASE = '\/api\/business\/erp';$/m, 'BASE 必须含 /api 前缀');
  assert.match(
    api,
    /export function fetchExpenseTypeNextCode\(\): Promise<NextCodePreview>/,
    '缺 fetchExpenseTypeNextCode',
  );
  assert.match(
    api,
    /`\$\{BASE\}\/expense-type\/next-code`/,
    '费用类型预览路径写成 ${BASE}/expense-type/next-code',
  );
  assert.match(
    api,
    /import type \{ NextCodePreview \} from '\.\/index'/,
    '响应体契约复用 index.ts 的 NextCodePreview',
  );
  assert.match(api, /type_code: string \| null;/, 'ExpenseTypePayload.type_code 要允许 null');
});

// ==================== 费用类型 ④ 页面：按钮在、编辑态禁用、保存后回填 ====================

test('费用类型·④页面：编码栏旁有「生成编码」按钮，编辑态禁用，保存后回填后端编码', () => {
  const vue = read('views/business/erp/expense-type/index.vue');
  assert.ok(vue.includes('生成编码'), '要有「生成编码」按钮文案');
  assert.ok(vue.includes('fetchExpenseTypeNextCode'), '要点预览接口');
  assert.ok(vue.includes('data-testid="expense-type-gen-code"'), '按钮要带 testid');
  assert.match(
    vue,
    /<t-button[\s\S]*?:disabled="!!editingId"[\s\S]*?data-testid="expense-type-gen-code"/,
    '按钮必须编辑态禁用',
  );
  assert.ok(vue.includes('handleGenCode'), '要有点击处理函数');
  assert.match(vue, /form\.type_code = preview\.code/, '预览号要填进编码框（可手改）');
  assert.match(
    vue,
    /v-model="form\.type_code"[\s\S]*?:disabled="!!editingId"/,
    '编码输入框必须编辑态只读',
  );
  assert.ok(
    vue.includes('form.type_code = created.type_code'),
    '保存后要把后端真正用的编码回填表单',
  );
  assert.ok(vue.includes('不点「生成编码」则保存时自动生成'), '要有 help 提示文案');
  // 页面上的「必填」提示文案必须已经改口（旧文案「大写字母开头（如 TRAVEL）」会误导）
  assert.ok(!vue.includes('大写字母开头'), '页面提示不能还写着「大写字母开头」');
});
