/**
 * 「批量动作成功后结果弹窗永远看不到」的源码契约测（t_cea00ecf 回归护栏）。
 *
 * # 缺陷形态
 *
 * `BatchActionBar` 的根节点是 `<div v-if="visible">`，结果弹窗是它的子节点。
 * `execute()` 成功收尾时先 `resultVisible.value = true`，紧接着
 * `emit('update:selected', [])` 清空勾选；一旦 `visible` 只判「有勾选 / 正在跑」，
 * 这一拍就变 false → 根节点连同结果弹窗一起被卸载，`resultVisible` 形同虚设。
 * 表现是：批量导出 / 批量过账跑完 200，界面上没有任何反馈（失败路径靠
 * `MessagePlugin.error` teleport 到 body，所以只有失败看得见）。
 *
 * # 为什么测源码
 *
 * 这条链是「计算属性 × 模板 v-if × 事件顺序」的组合，纯函数单测（`batch-action.ts`）
 * 碰不到，`vue-tsc` 也看不见。本仓既有姿势是直接读 SFC 断言结构
 * （见 `flow-canvas-style.test.ts` / `payable-drawer-refresh.test.ts`）。
 *
 * 除了子串断言，这里还把 `visible` 的**表达式本身抽出来求值**跑一张真值表：
 * 把 `|| resultVisible.value` 删掉，或把三个分支换成任意等价写法而漏掉一条，
 * 真值表那两条就会红。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const SFC_PATH = resolve(HERE, '../../src/components/BatchActionBar.vue');
const src = readFileSync(SFC_PATH, 'utf-8');

/** 取某个顶层函数的函数体（从 `function <name>(` 到下一个 `\n}`），找不到就 fail */
function bodyOf(source: string, name: string): string {
  const start = source.indexOf(`function ${name}(`);
  assert.notEqual(start, -1, `源码里找不到函数 ${name}()`);
  const end = source.indexOf('\n}\n', start);
  assert.notEqual(end, -1, `函数 ${name}() 的闭合大括号找不到`);
  return source.slice(start, end + 3);
}

/**
 * 把 `const visible = computed(() => …);` 里的表达式抽出来，用假的状态求值。
 * 取不到就直接 fail —— 说明判据被改写成了别的形态，本测需要同步更新。
 */
const visibleExpr = (() => {
  const match = src.match(/const visible = computed\(\s*\(\)\s*=>\s*([\s\S]*?)\s*\);/);
  assert.ok(match, '找不到 `const visible = computed(() => …);`：判据被改写了，请同步本条契约测');
  return match[1];
})();

type VisibleJudge = (
  props: { selected: unknown[] },
  running: { value: boolean },
  resultVisible: { value: boolean }
) => boolean;

const judgeVisible: VisibleJudge = new Function(
  'props',
  'running',
  'resultVisible',
  `return (${visibleExpr});`
) as VisibleJudge;

function visibleOf(selectedCount: number, running: boolean, resultVisible: boolean): boolean {
  return judgeVisible(
    { selected: Array.from({ length: selectedCount }, (_, i) => ({ id: `row-${i}` })) },
    { value: running },
    { value: resultVisible }
  );
}

test('visible 的三个分支：有勾选 / 正在跑 / 结果弹窗还没关', () => {
  assert.equal(visibleOf(2, false, false), true, '有勾选时工具栏必须在');
  assert.equal(visibleOf(0, true, false), true, '执行中工具栏必须在（进度条要看得见）');
  assert.equal(
    visibleOf(0, false, true),
    true,
    '结果弹窗开着时工具栏必须在：勾选已在 execute() 末尾被清空，漏掉这一条整个根节点会被 v-if 卸掉 → 结果永远看不到'
  );
  assert.equal(visibleOf(0, false, false), false, '没勾选、没在跑、弹窗也关了 → 工具栏隐藏（不占位）');
});

test('execute() 成功收尾的顺序：先 resultVisible = true，再清空勾选', () => {
  const execute = bodyOf(src, 'execute');
  const setVisibleAt = execute.indexOf('resultVisible.value = true');
  const clearSelectionAt = execute.indexOf("emit('update:selected', [])");
  assert.notEqual(setVisibleAt, -1, 'execute() 成功分支必须打开结果弹窗');
  assert.notEqual(clearSelectionAt, -1, 'execute() 结束要清空勾选（刷新不保持勾选）');
  assert.ok(
    setVisibleAt < clearSelectionAt,
    '必须先 resultVisible = true 再清空勾选，否则两者之间会有一帧「无勾选且弹窗未打开」→ 根节点卸载'
  );
});

test('结果弹窗的显隐由 resultVisible 受控，并接住关闭事件', () => {
  // data-testid 落在 <t-dialog> 本身，取它附近的一段做结构断言
  const marker = src.indexOf(':data-testid="`${testid}-result`"');
  assert.notEqual(marker, -1, '找不到结果弹窗的 data-testid 标记');
  const dialog = src.slice(Math.max(0, marker - 400), marker + 120);

  assert.match(dialog, /:visible="resultVisible"/, '结果弹窗的 visible 必须绑到 resultVisible');
  assert.match(
    dialog,
    /@update:visible="onResultVisibleChange"/,
    '关闭（×/遮罩/Esc）必须走 onResultVisibleChange：关掉时要顺手清 outcome'
  );
  assert.doesNotMatch(
    dialog,
    /v-model:visible="resultVisible"/,
    '别再用 v-model:visible：它在关闭时只回写布尔值，清不掉 outcome（会残留上一次的成败清单）'
  );
});

test('关掉结果弹窗会清掉本次 outcome（下一次动作不残留上一次结果）', () => {
  const handler = bodyOf(src, 'onResultVisibleChange');
  assert.match(handler, /resultVisible\.value\s*=\s*value/, '先把新的显隐写回 resultVisible');
  assert.match(handler, /outcome\.value\s*=\s*undefined/, '关闭时必须清 outcome：结果是一次性的');
});

test('结果弹窗的标题/正文仍然来自 resultHeadline / resultHint（导出任务要能被认出来）', () => {
  assert.match(src, /const headline = computed\(\(\) => \(outcome\.value \? resultHeadline\(/, '标题必须走 resultHeadline');
  assert.match(src, /const hint = computed\(\(\) => \(outcome\.value \? resultHint\(/, '正文必须走 resultHint');
});
