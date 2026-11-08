/**
 * CODE-08 单测：主数据编码「允许手工指定」开关（前端半边）。
 *
 * # 钉死四件事
 *
 * 1. **纯逻辑口径**：开关关闭 ⇒ 编码栏禁用 + 提示「编码由系统自动生成」；
 *    编辑态永远禁用（与开关无关）。这一层直接跑函数，不靠读源码。
 * 2. **策略接口路径写全 `/api`**：`${BASE}/master-data/code-policy`（漏前缀会命中 SPA
 *    fallback 返回 HTML —— 仓内高频坑）。
 * 3. **fail-open**：接口挂了按「允许手填」渲染（真正的闸在后端 400 + 66054），
 *    并把缓存 Promise 置空以便下次重试。
 * 4. **六个表单行为一致**：每个表单都要有「绑策略 → 输入框禁用 → 按钮 `v-if` → 提示文案」
 *    这四件事。少一处就是「开关关了仍能填」的破口，而 `vue-tsc` 抓不到模板里的漏写，
 *    所以这一段只能读源码钉（同 CODE-05/06/07 的做法）。
 *
 * 运行：`pnpm test:unit`
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  DEFAULT_ALLOW_MANUAL_CODE,
  MANUAL_CODE_FORBIDDEN_HINT,
  manualCodeDisabled,
  manualCodeHelp,
} from '../../src/views/business/erp/shared/code-policy-pure';

const HERE = dirname(fileURLToPath(import.meta.url));
const SRC = resolve(HERE, '../../src');

function read(relative: string): string {
  return readFileSync(join(SRC, relative), 'utf8');
}

// ==================== ① 纯逻辑 ====================

test('兜底值是「允许」（参数缺失 / 接口挂了都不该锁死编码框）', () => {
  assert.equal(DEFAULT_ALLOW_MANUAL_CODE, true);
});

test('manualCodeDisabled：编辑态或开关关闭都禁用，只有「新建 + 允许」才可填', () => {
  assert.equal(manualCodeDisabled(false, true), false, '新建 + 允许 → 可填');
  assert.equal(manualCodeDisabled(true, true), true, '编辑态 → 禁用');
  assert.equal(manualCodeDisabled(false, false), true, '开关关闭 → 禁用');
  assert.equal(manualCodeDisabled(true, false), true, '编辑态 + 开关关闭 → 禁用');
});

test('manualCodeHelp：开关关闭时给出「编码由系统自动生成」，打开时保留各页原文案', () => {
  assert.equal(manualCodeHelp(true, '不点「生成编码」则保存时自动生成'), '不点「生成编码」则保存时自动生成');
  const closed = manualCodeHelp(false, '不点「生成编码」则保存时自动生成');
  assert.equal(closed, MANUAL_CODE_FORBIDDEN_HINT);
  assert.match(closed, /编码由系统自动生成/, '关闭态必须说清楚编码由系统生成');
  assert.match(closed, /保留空/, '关闭态要告诉用户「保留空即可」');
});

// ==================== ② 策略接口 ====================

test('API：code-policy 接口存在且路径写全（${BASE} 已含 /api）', () => {
  const api = read('api/business/erp/index.ts');
  assert.match(api, /export function fetchMasterDataCodePolicy\(\): Promise<MasterDataCodePolicy>/, '缺 fetchMasterDataCodePolicy');
  assert.match(api, /`\$\{BASE\}\/master-data\/code-policy`/, '路径必须是 ${BASE}/master-data/code-policy');
  assert.match(api, /allow_manual_code: boolean/, '类型里要有 allow_manual_code');
});

test('策略加载：缓存 Promise + 失败 fail-open（catch 返回兜底值并清缓存）', () => {
  const loader = read('views/business/erp/shared/code-policy.ts');
  assert.match(loader, /if \(force \|\| policyPromise === null\)/, '必须带缓存（六页共用一次请求）');
  assert.match(loader, /\.catch\(\(\) => \{[\s\S]*?policyPromise = null;[\s\S]*?return DEFAULT_ALLOW_MANUAL_CODE;/, '失败必须清缓存并回退「允许」');
  assert.match(loader, /fetchMasterDataCodePolicy\(\)/, '必须真的调策略接口');
});

// ==================== ③ 六个表单的四件事 ====================

/** 表单相对路径（warehouse 的表单在组件里，不在 index.vue） */
const FORMS: Array<{ name: string; file: string }> = [
  { name: '供应商', file: 'views/business/erp/supplier/index.vue' },
  { name: '客户', file: 'views/business/erp/customer/index.vue' },
  { name: '物料', file: 'views/business/erp/material/index.vue' },
  { name: '仓库', file: 'views/business/erp/warehouse/components/WarehouseFormDialog.vue' },
  { name: '资产分类', file: 'views/business/erp/asset-category/index.vue' },
  { name: '费用类型', file: 'views/business/erp/expense-type/index.vue' },
];

for (const form of FORMS) {
  test(`${form.name}表单：绑策略 → 编码框按策略禁用 → 生成编码按钮 v-if → 关闭态提示`, () => {
    const vue = read(form.file);
    assert.match(
      vue,
      /from '(\.\.\/)+(shared\/code-policy|\.\.\/shared\/code-policy)'|from '@\/views\/business\/erp\/shared\/code-policy'/,
      '必须从 shared/code-policy 引策略',
    );
    assert.match(vue, /loadAllowManualCode/, '必须调 loadAllowManualCode');
    assert.match(vue, /const allowManualCode = ref\(true\)/, '必须有 allowManualCode 状态（乐观默认 true）');
    assert.match(
      vue,
      /:disabled="manualCodeDisabled\([^"]*allowManualCode\)"/,
      '编码框禁用条件必须走 manualCodeDisabled(isEdit, allowManualCode)',
    );
    assert.match(vue, /v-if="allowManualCode"[\s\S]{0,400}生成编码/, '「生成编码」按钮必须 v-if 绑策略（关闭时隐藏）');
    assert.match(vue, /manualCodeHelp\(allowManualCode,/, '提示文案必须走 manualCodeHelp（关闭时改成「编码由系统自动生成」）');
    assert.match(vue, /allowManualCode\.value = (await |allow)/, '打开页面 / 弹窗时必须真的把策略写进 allowManualCode');
    assert.match(vue, /loadAllowManualCode\(\)/, '必须真的去读策略（缓存命中是纯内存读）');
  });
}
