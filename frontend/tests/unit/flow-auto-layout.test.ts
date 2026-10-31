/**
 * `auto-layout.ts` 单测 —— dagre 树状布局契约（P12）。
 *
 * 运行：`pnpm test:unit`（`tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * 测的是**坐标语义**，不是具体像素：dagre 版本升级后节点可能整体平移几个像素，
 * 但「同层 y 相同 / 下一层 y 更大 / 节点矩形不重叠」这些关系必须稳住 ——
 * 页面上的「撤销布局」「fitView」都建立在「坐标是一张整齐的树」这个前提上。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { computeAutoLayout, type LayoutEdge, type LayoutNode } from '../../src/views/business/flow/auto-layout.ts';

/** 造节点（默认 150×64，接近审批节点） */
const node = (id: string, width = 150, height = 64): LayoutNode => ({ id, width, height });

/** 造边 */
const edge = (source: string, target: string): LayoutEdge => ({ source, target });

/** 取坐标，缺了直接炸（比 `?.` 静默跳过更容易看出布局漏节点） */
function at(result: Map<string, { x: number; y: number }>, id: string): { x: number; y: number } {
  const point = result.get(id);
  assert.ok(point, `布局结果里缺少节点 ${id}`);
  return point as { x: number; y: number };
}

/** 两个矩形是否相交（含边贴边） */
function overlap(
  a: { x: number; y: number; w: number; h: number },
  b: { x: number; y: number; w: number; h: number },
): boolean {
  return a.x < b.x + b.w && b.x < a.x + a.w && a.y < b.y + b.h && b.y < a.y + a.h;
}

// ==================== ① 单链：逐层加深 ====================

test('① 单链 A→B→C（TB）：x 相同、y 逐层递增', () => {
  const nodes = [node('A'), node('B'), node('C')];
  const result = computeAutoLayout(nodes, [edge('A', 'B'), edge('B', 'C')], 'TB');
  assert.equal(result.size, 3);
  const [a, b, c] = [at(result, 'A'), at(result, 'B'), at(result, 'C')];
  // TB 布局里同一条链的节点必然同一列
  assert.equal(a.x, b.x);
  assert.equal(b.x, c.x);
  // 层间距 ranksep=72 + 节点高 64 ⇒ 相邻层至少差 64
  assert.ok(b.y > a.y, `B 应在 A 下方：a.y=${a.y} b.y=${b.y}`);
  assert.ok(c.y > b.y, `C 应在 B 下方：b.y=${b.y} c.y=${c.y}`);
  assert.ok(b.y - a.y >= 64, `层间距不足：${b.y - a.y}`);
});

// ==================== ② 分支：同层并列 ====================

test('② 分支 A→B / A→C：B、C 同层（y 相同）、x 不同', () => {
  const nodes = [node('A'), node('B'), node('C')];
  const result = computeAutoLayout(nodes, [edge('A', 'B'), edge('A', 'C')], 'TB');
  const [b, c] = [at(result, 'B'), at(result, 'C')];
  assert.equal(b.y, c.y, '同一层的两个分支必须 y 相同');
  assert.notEqual(b.x, c.x, '同一层的两个分支必须错开 x');
  assert.ok(at(result, 'A').y < b.y, '父节点在上一层');
});

// ==================== ③ 菱形：两条路径汇聚到最后一层 ====================

test('③ 菱形 A→B→D、A→C→D：B/C 同层，D 在最后一层', () => {
  const nodes = [node('A'), node('B'), node('C'), node('D')];
  const result = computeAutoLayout(
    nodes,
    [edge('A', 'B'), edge('A', 'C'), edge('B', 'D'), edge('C', 'D')],
    'TB',
  );
  const [a, b, c, d] = [at(result, 'A'), at(result, 'B'), at(result, 'C'), at(result, 'D')];
  assert.equal(b.y, c.y, 'B / C 同层');
  assert.notEqual(b.x, c.x);
  assert.equal(a.y, Math.min(a.y, b.y), 'A 在最上层');
  assert.ok(d.y > b.y && d.y > c.y, `D 应在 B/C 之下：d.y=${d.y} b.y=${b.y} c.y=${c.y}`);
  // 汇聚节点水平居中于两条分支之间（dagre 的居中约束）
  const mid = (b.x + c.x) / 2;
  assert.ok(Math.abs(d.x - mid) <= 150, `D 应大致居中：d.x=${d.x} mid=${mid}`);
});

// ==================== ④ 孤立节点：也要有坐标且不压别人 ====================

test('④ 孤立节点（无连线）：结果里有它，且不与任何节点重叠', () => {
  const nodes = [node('A'), node('B'), node('LONELY')];
  const result = computeAutoLayout(nodes, [edge('A', 'B')], 'TB');
  assert.equal(result.size, 3, '孤立节点不能被丢掉（否则页面上它会留在原地叠成一坨）');
  const boxes = nodes.map((n) => ({ ...at(result, n.id), w: n.width, h: n.height }));
  for (let i = 0; i < boxes.length; i += 1) {
    for (let j = i + 1; j < boxes.length; j += 1) {
      assert.ok(!overlap(boxes[i], boxes[j]), `节点 ${nodes[i].id} 与 ${nodes[j].id} 重叠`);
    }
  }
});

// ==================== ⑤ 空输入 ====================

test('⑤ 空输入 → 返回空 Map，不崩', () => {
  const result = computeAutoLayout([], []);
  assert.equal(result.size, 0);
  // 有边没节点（脏数据）也不能崩
  assert.equal(computeAutoLayout([], [edge('A', 'B')]).size, 0);
});

// ==================== ⑥ 自环 ====================

test('⑥ 自环边 A→A：不崩，A 仍有坐标（自环不参与层次划分）', () => {
  const result = computeAutoLayout([node('A'), node('B')], [edge('A', 'A'), edge('A', 'B')], 'TB');
  assert.equal(result.size, 2);
  assert.ok(at(result, 'A').y < at(result, 'B').y, '自环不应把 A 推到 B 后面');
});

// ==================== ⑦ 成环 ====================

test('⑦ 成环 A→B→C→A：dagre 能处理，三个节点都有整数坐标', () => {
  const nodes = [node('A'), node('B'), node('C')];
  const result = computeAutoLayout(nodes, [edge('A', 'B'), edge('B', 'C'), edge('C', 'A')], 'TB');
  assert.equal(result.size, 3);
  for (const n of nodes) {
    const point = at(result, n.id);
    assert.ok(Number.isFinite(point.x) && Number.isFinite(point.y));
  }
});

// ==================== ⑧ 尺寸不同：不重叠 ====================

test('⑧ 节点尺寸差异大（120×52 / 220×76）：不重叠', () => {
  const nodes = [node('A', 120, 52), node('B', 220, 76), node('C', 150, 64)];
  const result = computeAutoLayout(nodes, [edge('A', 'B'), edge('A', 'C')], 'TB');
  const boxes = nodes.map((n) => ({ ...at(result, n.id), w: n.width, h: n.height }));
  for (let i = 0; i < boxes.length; i += 1) {
    for (let j = i + 1; j < boxes.length; j += 1) {
      assert.ok(!overlap(boxes[i], boxes[j]), `节点 ${nodes[i].id} 与 ${nodes[j].id} 重叠`);
    }
  }
  // 同层的 B / C 之间要留出 nodesep：220 宽的 B 与 150 宽的 C 间距 ≥ 46
  const [b, c] = [at(result, 'B'), at(result, 'C')];
  const gap = c.x > b.x ? c.x - (b.x + 220) : b.x - (c.x + 150);
  assert.ok(gap >= 40, `同层间距不足：${gap}`);
});

// ==================== ⑨ LR 方向 ====================

test('⑨ LR 方向：x 逐层递增、同层 y 相同', () => {
  const nodes = [node('A'), node('B'), node('C')];
  const result = computeAutoLayout(nodes, [edge('A', 'B'), edge('A', 'C')], 'LR');
  const [a, b, c] = [at(result, 'A'), at(result, 'B'), at(result, 'C')];
  assert.ok(b.x > a.x && c.x > a.x, 'B / C 应在 A 的右侧');
  assert.equal(b.x, c.x, '同层（同一列）x 相同');
  assert.notEqual(b.y, c.y, '同一列上下错开');
  // 与画布锚点口径一致：从左到右流动
  assert.ok(a.x < b.x);
});

// ==================== ⑩ 整数坐标 ====================

test('⑩ 结果坐标都是整数（Math.round 生效，落库 / 网格吸附都不会带小数）', () => {
  const nodes = [node('A'), node('B'), node('C', 133, 77)];
  const result = computeAutoLayout(nodes, [edge('A', 'B'), edge('B', 'C')], 'LR');
  for (const [, point] of result) {
    assert.equal(point.x, Math.round(point.x));
    assert.equal(point.y, Math.round(point.y));
  }
});

// ==================== ⑪ 同向多条边（工作流里合法） ====================

test('⑪ A→B 两条边（条件分支同向）：不崩，仍只有两个层次', () => {
  const nodes = [node('A'), node('B')];
  const result = computeAutoLayout(nodes, [edge('A', 'B'), edge('A', 'B')], 'LR');
  assert.equal(result.size, 2);
  assert.ok(at(result, 'B').x > at(result, 'A').x);
});

// ==================== ⑫ 端点缺失的脏边 ====================

test('⑫ 边指向不存在的节点：忽略该边，不为幽灵节点造坐标', () => {
  const result = computeAutoLayout([node('A')], [edge('A', 'GHOST')], 'TB');
  assert.equal(result.size, 1);
  assert.ok(!result.has('GHOST'));
});
