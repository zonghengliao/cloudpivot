/**
 * 采购申请（P2-02）建单明细：**源码契约**单测（t_8dcb79d5）。
 *
 * 运行：`pnpm test:unit`（零新依赖、无 DOM），也可以单跑
 * `npx tsx --test tests/unit/erp-purchase-requisition-lines-slot.test.ts`。
 *
 * # 钉的是什么
 *
 * TDesign `t-table` 按列的 `colKey` 去找单元格插槽 `#<colKey>`。写成 `#lineMaterial`
 * 这种自定义名字时：**没有任何编译期或运行期报错**，槽子直接被忽略 ——
 * 明细行渲染成裸值（物料列空白、数量显示 `1` 这串文本、下拉/日期/删除链接全都没有）。
 * `vue-tsc`（pnpm typecheck）与 `pnpm build` 对这种写法都是绿的，所以把它钉成源码契约：
 *
 * | 组 | 钉住什么 |
 * |----|----------|
 * | ① | 建单明细的插槽名集合 **必须逐字等于** `lineColumns` 的 `colKey` 集合 |
 * | ② | 每个带输入控件（t-select / t-input-number / t-date-picker）的槽子必须挂 `data-testid`，e2e 才点得到 |
 * | ③ | 反证：`#lineMaterial` / `#lineQty` / `#linePrice` 这套旧命名不许再出现 |
 *
 * 真浏览器验收在 `frontend/e2e/m4-prq-create-lines.spec.ts`（本单测不能替代它：
 * 单测只读源码，TDesign 到底认不认这个名字只有 DOM 说得准）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const PAGE = resolve(HERE, '../../src/views/business/erp/purchase-requisition/index.vue');
const SOURCE = readFileSync(PAGE, 'utf8');

/** 取「建单明细」那张表的模板片段（按 testid 定位，不靠行号） */
function linesTableBlock(): string {
  const anchor = SOURCE.indexOf('data-testid="prq-lines-table"');
  assert.ok(anchor > 0, 'index.vue 里找不到 data-testid="prq-lines-table"');
  const open = SOURCE.lastIndexOf('<t-table', anchor);
  const close = SOURCE.indexOf('</t-table>', anchor);
  assert.ok(open >= 0 && close > anchor, '建单明细表的 <t-table> 标签不完整');
  return SOURCE.slice(open, close);
}

/** 表格里的自定义单元格插槽名（`<template #xxx="{ row }">`） */
function slotNames(block: string): string[] {
  return [...block.matchAll(/<template\s+#([A-Za-z0-9_]+)=/g)].map((m) => m[1]);
}

/** 每个插槽的片段（用于判断里面有没有输入控件、有没有 testid） */
function slotBody(block: string, name: string): string {
  const m = new RegExp(`<template\\s+#${name}=[^>]*>`).exec(block);
  assert.ok(m, `槽子 #${name} 的起始标签没找到`);
  const start = m.index + m[0].length;
  const end = block.indexOf('</template>', start);
  return block.slice(start, end);
}

/** `lineColumns` 的 colKey 集合 */
function lineColumnKeys(): string[] {
  const m = /const lineColumns[^=]*=\s*\[/.exec(SOURCE);
  assert.ok(m, 'index.vue 里找不到 const lineColumns');
  const start = SOURCE.indexOf('[', m.index);
  const end = SOURCE.indexOf('];', start);
  const body = SOURCE.slice(start, end + 1);
  return [...body.matchAll(/colKey:\s*'([^']+)'/g)].map((x) => x[1]);
}

const INPUT_TAGS = ['t-select', 't-input-number', 't-date-picker'];

test('① 建单明细的插槽名必须逐字等于 lineColumns 的 colKey', () => {
  const block = linesTableBlock();
  const keys = lineColumnKeys();
  const slots = slotNames(block);

  assert.deepEqual(
    [...keys].sort(),
    ['demand_date', 'est_price', 'material_id', 'ops', 'qty', 'suggest_supplier_id', 'tax_rate'],
    'lineColumns 的 colKey 变了：改列名时插槽名必须一起改（本断言的 diff 会指出漏了哪个）',
  );
  assert.deepEqual(
    [...slots].sort(),
    [...keys].sort(),
    '建单明细的单元格插槽名必须等于 colKey，否则 TDesign 静默忽略槽子（明细行没有输入控件）',
  );
});

test('② 带输入控件的槽子必须挂 data-testid（e2e 才点得到）', () => {
  const block = linesTableBlock();
  const withInputs = slotNames(block).filter((name) => {
    const body = slotBody(block, name);
    return INPUT_TAGS.some((tag) => body.includes(`<${tag}`));
  });
  // 6 个控件槽：物料 / 数量 / 单价 / 税率 / 需求日期 / 建议供应商（第 7 个 ops 是删除链接）
  assert.equal(withInputs.length, 6, `带输入控件的槽子应为 6 个，实际 ${withInputs.join(',')}`);
  for (const name of withInputs) {
    const body = slotBody(block, name);
    assert.match(body, /data-testid="prq-line-[a-z]+"/, `槽子 #${name} 里的输入控件没挂 data-testid`);
  }
});

test('③ 反证：旧的自定义命名（#lineMaterial / #lineQty …）不许再出现', () => {
  const block = linesTableBlock();
  for (const legacy of ['lineMaterial', 'lineQty', 'linePrice', 'lineTax', 'lineDemand', 'lineSupplier', 'lineOps']) {
    assert.ok(
      !slotNames(block).includes(legacy),
      `建单明细里又出现了 #${legacy}：槽子会静默失效，明细行等于没有输入控件`,
    );
  }
});
