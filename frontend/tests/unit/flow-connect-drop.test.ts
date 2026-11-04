/**
 * `resolveConnectDrop` 单测 —— 连线落点解析（VUE-FLOW-2B 的真机 bug 根因修复）。
 *
 * 运行：`pnpm test:unit`。
 *
 * # 为什么要钉这个函数
 *
 * Vue Flow 内部只认「松手点在目标 Handle 的 `connectionRadius`（默认 20 画布 px）内」，
 * 而画布的落点高亮（`connectTargetKey`）用的是**整张卡片**（老画布口径）。两者不一致时
 * 用户会看到「卡片亮起绿色虚线 = 可以落点」，松手却什么也没发生（Vue Flow 静默 return）。
 * 所以 `onConnectEnd` 用本函数按老画布三级判定补一次 `connect`。
 *
 * 这些用例就是「老画布 `findDropTarget` 的行为契约」，改口径必须先改这里。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { resolveConnectDrop } from '../../src/views/business/flow/graph.ts';
import type { FlowNode } from '../../src/api/business/flow-api.ts';

/** 造节点（只需 key/type/x/y 参与命中测试，其余字段给最小合法值） */
function node(key: string, type: string, x: number, y: number): FlowNode {
  return { key, name: key, type, x, y, sort_order: 0 } as unknown as FlowNode;
}

// 尺寸取自 flow-node-view 的 nodeSize：START/END 96×44、条件 132×84、审批 150×64
const start = node('s', 'START', 0, 0); // 96×44  → 右边缘 x=96，入口锚点 (0, 22)
const task = node('t', 'APPROVAL', 300, 0); // 150×64 → 卡片 [300,450] × [0,64]，入口锚点 (300, 32)
const cond = node('c', 'CONDITION', 600, 0); // 132×84

// ==================== ① 压在别的卡片上 ====================

test('① 落点压在目标卡片内任意位置 → 返回目标节点（不要求靠近句柄）', () => {
  // 卡片中心、左边缘内侧、右上角 —— 三个位置都该命中（Vue Flow 只认 20 画布 px）
  for (const point of [
    { x: 375, y: 32 },
    { x: 302, y: 2 },
    { x: 448, y: 62 },
  ]) {
    assert.equal(resolveConnectDrop([start, task], point, 's')?.key, 't', `落点 ${JSON.stringify(point)} 应命中 t`);
  }
});

test('② 落点紧贴卡片左边缘外 5px（空白）→ 被入口锚点 40px 内吸附；右侧外沿则不吸附', () => {
  // 吸附半径是围着**入口锚点**（卡片左边中点）算的，与老画布一致
  assert.equal(resolveConnectDrop([start, task], { x: 295, y: 32 }, 's')?.key, 't');
  // 卡片右侧外沿离入口锚点 151px → 不吸附（右出左进的语义下，落点在右侧没有意义）
  assert.equal(resolveConnectDrop([start, task], { x: 451, y: 32 }, 's'), null);
});

// ==================== ③ 起点自己 ====================

test('③ 落点回到起点卡片上 → 返回起点（交给页面提示「不能连到自己」）', () => {
  const hit = resolveConnectDrop([start, task], { x: 48, y: 22 }, 's');
  assert.equal(hit?.key, 's');
});

test('④ 起点与目标重叠时，优先返回「别的」节点（不能因为自环提示吞掉合法落点）', () => {
  const overlapping = node('o', 'APPROVAL', 20, 0); // 与 start 重叠
  const hit = resolveConnectDrop([start, overlapping], { x: 40, y: 22 }, 's');
  assert.equal(hit?.key, 'o');
});

// ==================== ⑤ 空白处 ====================

test('⑤ 落点在空白处、且离所有入口锚点都 > 40px → null（不乱连）', () => {
  assert.equal(resolveConnectDrop([start, task], { x: 200, y: 200 }, 's'), null);
});

test('⑥ 落点在空白处、离入口锚点 40px 内 → 吸附最近节点（老画布的宽容度）', () => {
  // task 入口锚点 (300,32)：距离 30 → 吸附；另一个节点更远
  assert.equal(resolveConnectDrop([start, task, cond], { x: 270, y: 32 }, 's')?.key, 't');
  // 两个候选都在 40 内时取更近的那个
  const near = node('n', 'APPROVAL', 0, 200); // 入口锚点 (0, 232)
  assert.equal(resolveConnectDrop([start, near], { x: 0, y: 250 }, 's')?.key, 'n');
});

// ==================== ⑦ 边界输入 ====================

test('⑦ 没有起点（fromKey=null）：只走「压在别的卡片上」这一级，不做自环 / 吸附', () => {
  assert.equal(resolveConnectDrop([start, task], { x: 375, y: 32 }, null)?.key, 't');
  assert.equal(resolveConnectDrop([start, task], { x: 48, y: 22 }, null)?.key, 's');
  assert.equal(resolveConnectDrop([start, task], { x: 270, y: 32 }, null), null);
});

test('⑧ fromKey 不在节点表里（脏数据）：不抛错，仍能命中其它卡片', () => {
  assert.equal(resolveConnectDrop([start, task], { x: 375, y: 32 }, 'ghost')?.key, 't');
  assert.equal(resolveConnectDrop([start, task], { x: -999, y: -999 }, 'ghost'), null);
});

test('⑨ 卡片边界是闭区间（贴边走也算命中，避免差 1px 落空）', () => {
  // task 卡片 [300,450]×[0,64]
  assert.equal(resolveConnectDrop([task], { x: 300, y: 0 }, 's')?.key, 't');
  assert.equal(resolveConnectDrop([task], { x: 450, y: 64 }, 's')?.key, 't');
});
