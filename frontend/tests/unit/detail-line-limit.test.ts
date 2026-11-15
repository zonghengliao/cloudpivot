/**
 * P1-04 单测：单据明细超 200 行的「明确拒绝 + 可操作提示」。
 *
 * # 钉死的口径
 *
 * 1. **文案**：超限提示必须同时说清「上限 / 当前行数 / 下一步动作（拆单）」，
 *    与后端 `backend/crates/cloudpivot-kernel/src/business/shared/doc_lines.rs` 的 `too_many_lines()` 同口径；
 *    并**反向断言**不含「导入向导」—— 导入通道里没有这 7 个单据（决策文档 §1 F1），
 *    谁把这张空头支票改回来，测试就红；
 * 2. **边界**：恰好 200 行合法，201 行才提示（`>` 而不是 `>=`）—— 差一行就误报会
 *    让「正好 200 行」的老单据莫名冒出黄条；
 * 3. **覆盖面**：7 个单据页都要挂同一个组件（入库 / 采购 / 调拨 / 出库 / 询价 /
 *    采购申请 / 采购退货），漏一个就有一页的用户只知道「保存失败」；
 * 4. **不越界**：盘点单上限是 2000（卡面明确保持不动），**不许**给它挂 200 的提示；
 * 5. **只给文案**：黄条不再带任何跳转（「打开导入向导」已按决策文档 §3.4 删掉，
 *    点进去找不到单据 = 二次挫折）。
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
  detailLineLimitMessage,
  overDetailLineLimit,
  splitDocumentHint,
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

test('超限文案三段齐全：上限 / 当前行数 / 拆单动作', () => {
  const message = detailLineLimitMessage(301);
  assert.ok(message.includes('明细最多 200 行'), message);
  assert.ok(message.includes('当前 301 行'), message);
  assert.ok(message.includes('拆成多张单据提交'), `必须给出用户能做的动作：${message}`);
  assert.ok(message.includes('每张不超过 200 行'), `要说清每张拆到多少：${message}`);
  // 反向断言：导入向导里没有这 7 个单据，不许拿它当出口（谁改回去谁红）。
  assert.ok(!message.includes('导入向导'), `不许再拿导入向导当出口：${message}`);
  assert.ok(!message.includes('2 万行'), `也不许暗示通道能接 2 万行明细分：${message}`);
});

test('引导文案与后端 `doc_lines::SPLIT_DOC_HINT` 逐字一致', () => {
  assert.equal(splitDocumentHint(), '请拆成多张单据提交，每张不超过 200 行。');
});

test('文案里没有导入向导残留（前端不再引用导入向导路由名）', () => {
  const source = readSource('views/business/erp/shared/detail-line-limit.ts');
  assert.ok(
    !source.includes('IMPORT_WIZARD_ROUTE_NAME'),
    '导入向导路由名常量应随入口一起删除（决策文档 §3.4）',
  );
  assert.ok(!source.includes('importWizardHint'), '旧函数名 importWizardHint 必须改名');
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

test('盘点单文案与后端同口径：当前 N 行 + 分多次抽盘（不写拆单/导入向导）', () => {
  const logic = readSource('views/business/erp/stock-take/stock-take-logic.ts');
  assert.ok(
    logic.includes('STOCK_TAKE_TOO_MANY_HINT = \'请按仓库或物料分类分多次抽盘。\''),
    '盘点尾句要与后端 `stocktake_dto::STOCK_TAKE_TOO_MANY_HINT` 逐字一致',
  );
  assert.ok(
    logic.includes('抽盘明细最多 ${STOCK_TAKE_MAX_LINES} 行（当前 ${form.items.length} 行）'),
    '盘点用「当前 N 行」，不能再用「收到 N 行」',
  );
  assert.ok(!logic.includes('收到'), '「收到」是纯技术口吻，已统一为「当前」');
});

// ==================== ③ 组件自身的口径 ====================

test('提示组件只在超限时渲染，且是 warning 主题', () => {
  const component = readSource('views/business/erp/shared/DetailLineLimitAlert.vue');
  assert.ok(component.includes('v-if="over"'), '不该在没超限时占位');
  assert.ok(component.includes('theme="warning"'), '这是提示不是报错（保存才拦）');
});

test('提示组件不再带任何跳转（只给文案）', () => {
  const component = readSource('views/business/erp/shared/DetailLineLimitAlert.vue');
  assert.ok(!component.includes('router-link'), '黄条不该有跳转（决策文档 §3.4）');
  assert.ok(!component.includes('打开导入向导'), '「打开导入向导」入口已删');
  assert.ok(!component.includes('IMPORT_WIZARD_ROUTE_NAME'), '路由名常量引用要一起删掉');
});

test('提示组件不阻断编辑：只读 count/limit，不 emit、不 disable 任何东西', () => {
  const component = readSource('views/business/erp/shared/DetailLineLimitAlert.vue');
  const script = component.slice(component.indexOf('<script setup'), component.indexOf('</script>'));
  assert.ok(!script.includes('defineEmits'), '提示组件不该有 emit（不阻断编辑）');
  assert.ok(!script.includes('defineProps<{ count: number }>'), '只读 props 的口径不能被改坏');
});
