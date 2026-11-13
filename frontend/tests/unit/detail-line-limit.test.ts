/**
 * P1-04 单测：单据明细超 200 行的「明确拒绝 + 可操作提示」。
 *
 * # 钉死的口径
 *
 * 1. **文案**：超限提示必须同时说清「上限 / 当前行数 / 该走哪条通道（导入向导）」，
 *    与后端 `backend/src/business/shared/doc_lines.rs` 的 `too_many_lines()` 同口径；
 * 2. **边界**：恰好 200 行合法，201 行才提示（`>` 而不是 `>=`）—— 差一行就误报会
 *    让「正好 200 行」的老单据莫名冒出黄条；
 * 3. **覆盖面**：7 个单据页都要挂同一个组件（入库 / 采购 / 调拨 / 出库 / 询价 /
 *    采购申请 / 采购退货），漏一个就有一页的用户只知道「保存失败」；
 * 4. **不越界**：盘点单上限是 2000（卡面明确保持不动），**不许**给它挂 200 的提示。
 *
 * 这些断言都是读源码文本 —— 组件在 SFC 里，没有运行时可以渲染；页面契约靠文本断言
 * 是仓库既有做法（见 `tests/unit/attachment-gate-pages.test.ts`）。
 */
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { test } from 'node:test';
import { fileURLToPath } from 'node:url';

import {
  DETAIL_MAX_LINES,
  IMPORT_WIZARD_ROUTE_NAME,
  detailLineLimitMessage,
  importWizardHint,
  overDetailLineLimit,
} from '../../src/views/business/erp/shared/detail-line-limit.ts';

const HERE = dirname(fileURLToPath(import.meta.url));
const SRC = resolve(HERE, '../../src');

function readSource(relative: string): string {
  return readFileSync(join(SRC, relative), 'utf8');
}

// ==================== ① 纯函数：文案与边界 ====================

test('默认上限与后端 MAX_LINES 同值（200）', () => {
  assert.equal(DETAIL_MAX_LINES, 200);
});

test('恰好 200 行不提示，201 行才提示', () => {
  assert.equal(overDetailLineLimit(0), false, '空明细不该提示');
  assert.equal(overDetailLineLimit(DETAIL_MAX_LINES), false, '恰好 200 行合法，不能误报');
  assert.equal(overDetailLineLimit(DETAIL_MAX_LINES + 1), true, '201 行必须提示');
});

test('上限可跟单据走（盘点 2000 走同一份函数）', () => {
  assert.equal(overDetailLineLimit(2000, 2000), false);
  assert.equal(overDetailLineLimit(2001, 2000), true);
  assert.ok(detailLineLimitMessage(2001, 2000).includes('最多 2000 行'));
});

test('超限文案三段齐全：上限 / 当前行数 / 导入向导通道', () => {
  const message = detailLineLimitMessage(301);
  assert.ok(message.includes('明细最多 200 行'), message);
  assert.ok(message.includes('当前 301 行'), message);
  assert.ok(message.includes('导入向导'), `必须给出可操作通道：${message}`);
  assert.ok(message.includes('业务 → 通用底座 → 导入向导'), `要写清菜单怎么走：${message}`);
  assert.ok(message.includes('2 万行'), `要说清通道容量：${message}`);
});

test('引导文案与后端 `doc_lines::IMPORT_WIZARD_HINT` 逐字一致', () => {
  assert.equal(
    importWizardHint(),
    '大批量请用「导入向导」：业务 → 通用底座 → 导入向导（导入通道支持 2 万行）',
  );
});

test('导入向导链接指向真实路由名（改了路由名这里会红）', () => {
  assert.equal(IMPORT_WIZARD_ROUTE_NAME, 'BusinessImpexpImport');
  const router = readSource('router/index.ts');
  assert.ok(router.includes(`name: '${IMPORT_WIZARD_ROUTE_NAME}'`), '路由表里必须有导入向导');
});

// ==================== ② 页面契约：7 个单据都要挂 ====================

/** 单据页 → 明细行数的绑定表达式（各页状态字段名不同，绑定必须指到**明细数组**）。 */
const PAGES: ReadonlyArray<readonly [string, string, string]> = [
  ['入库单', 'views/business/erp/stockin/index.vue', ':count="form.items.length"'],
  ['采购订单', 'views/business/erp/purchase/index.vue', ':count="form.items.length"'],
  ['调拨单', 'views/business/erp/transfer/index.vue', ':count="form.items.length"'],
  ['出库单', 'views/business/erp/stock-out/index.vue', ':count="form.items.length"'],
  ['询价单', 'views/business/erp/price-inquiry/index.vue', ':count="createForm.items.length"'],
  ['采购申请', 'views/business/erp/purchase-requisition/index.vue', ':count="createForm.lines.length"'],
  ['采购退货', 'views/business/erp/purchase-return/index.vue', ':count="source.items.length"'],
];

for (const [name, path, binding] of PAGES) {
  test(`${name}：明细表上方挂了超限提示，且绑定到明细数组长度`, () => {
    const page = readSource(path);
    assert.ok(
      page.includes("import DetailLineLimitAlert from '@/views/business/erp/shared/DetailLineLimitAlert.vue';"),
      `${name} 必须引入共享提示组件（别自己拼句子）`,
    );
    assert.ok(page.includes('<DetailLineLimitAlert'), `${name} 模板里必须有提示组件`);
    assert.ok(page.includes(binding), `${name} 的 :count 必须绑定明细数组长度（找不到 ${binding}）`);
    assert.ok(
      /testid="[a-z-]*line-limit"/.test(page),
      `${name} 的提示组件必须有稳定的 testid（e2e 定位用）`,
    );
  });
}

test('盘点单不挂 200 行提示（上限 2000，卡面明确保持不动）', () => {
  const page = readSource('views/business/erp/stock-take/index.vue');
  assert.ok(!page.includes('<DetailLineLimitAlert'), '盘点单上限 2000，不许挂 200 行提示');
});

// ==================== ③ 组件自身的口径 ====================

test('提示组件：只在超限时渲染，且带「打开导入向导」入口', () => {
  const component = readSource('views/business/erp/shared/DetailLineLimitAlert.vue');
  assert.ok(component.includes('v-if="over"'), '不该在没超限时占位');
  assert.ok(component.includes('theme="warning"'), '这是提示不是报错（保存才拦）');
  assert.ok(component.includes('打开导入向导'), '必须给一个可点的入口，不能只说「请用导入向导」');
  assert.ok(component.includes(':to="{ name: IMPORT_WIZARD_ROUTE_NAME }"'), '入口要指向真实路由名');
});

test('提示组件不阻断编辑：只读 count/limit，不 emit、不 disable 任何东西', () => {
  const component = readSource('views/business/erp/shared/DetailLineLimitAlert.vue');
  const script = component.slice(component.indexOf('<script setup'), component.indexOf('</script>'));
  assert.ok(!script.includes('defineEmits'), '提示组件不该有 emit（不阻断编辑）');
  assert.ok(!script.includes("defineProps<{ count: number }>"), '只读 props 的口径不能被改坏');
});
