/**
 * `rounded-path.ts` 单测 —— 正交折线转角圆化（P12 视觉升级）。
 *
 * 运行：`pnpm test:unit`。
 *
 * 圆角是「在 `graph.ts` 已有正交折线的基础上插二次贝塞尔」，最容易出错的地方有三处：
 * 拐角数量（会不会把直线也切成圆角）、半径是否越过段长（会把线画成回头钩）、
 * 以及脏点（重合点 / 单点）会不会产出 `NaN` 的 `d`。这里逐条钉住。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  ROUND_RADIUS,
  buildRoundedPath,
  labelTextWidth,
  parsePathPoints,
  roundedEdgePath,
} from '../../src/views/business/flow/components/rounded-path.ts';

/** 数一段 `d` 里的 `Q` 个数（= 圆角个数） */
function countQuads(d: string): number {
  return (d.match(/Q/g) ?? []).length;
}

/** 取 `d` 里所有数字（用来断言半径 / 合法性） */
function numbers(d: string): number[] {
  return (d.match(/-?\d+(?:\.\d+)?/g) ?? []).map(Number);
}

// ==================== ⓪ `d` 解析 ====================

test('⓪ 解析正交 `d`：M/L 一律按绝对坐标取点，支持小数与负数', () => {
  assert.deepEqual(parsePathPoints('M 0 0 L 100 0 L 100 40 L 200 40'), [
    { x: 0, y: 0 },
    { x: 100, y: 0 },
    { x: 100, y: 40 },
    { x: 200, y: 40 },
  ]);
  assert.deepEqual(parsePathPoints('M -10.5 20 L 0.25 -3 L 5 5'), [
    { x: -10.5, y: 20 },
    { x: 0.25, y: -3 },
    { x: 5, y: 5 },
  ]);
  // 空串 / 脏串不抛错，能解析多少算多少
  assert.deepEqual(parsePathPoints(''), []);
  assert.deepEqual(parsePathPoints('M 10 20'), [{ x: 10, y: 20 }]);
});

// ==================== ① 直线无圆角 ====================

test('① 两点直线 → path 里没有 Q（直线不切圆角）', () => {
  const d = buildRoundedPath([
    { x: 0, y: 0 },
    { x: 120, y: 0 },
  ]);
  assert.equal(countQuads(d), 0);
  assert.equal(d, 'M 0 0 L 120 0');
});

test('①-b 三点共线（折线但同向）→ 也不切圆角', () => {
  const d = buildRoundedPath([
    { x: 0, y: 0 },
    { x: 60, y: 0 },
    { x: 120, y: 0 },
  ]);
  assert.equal(countQuads(d), 0);
});

// ==================== ② 一个拐角 ====================

test('② 一个直角拐点 → 恰好 1 个 Q，半径 ≤ 8', () => {
  const d = buildRoundedPath([
    { x: 0, y: 0 },
    { x: 100, y: 0 },
    { x: 100, y: 80 },
  ]);
  assert.equal(countQuads(d), 1, `应只有 1 个圆角，实际 d=${d}`);
  // 圆角起点 = 拐点前 r 处 → 取「Q 之前那个 L」的 x，r = 100 - 该 x
  const beforeQuad = d.match(/L\s*(-?\d+(?:\.\d+)?)\s*(-?\d+(?:\.\d+)?)\s*Q/);
  assert.ok(beforeQuad, `d 里找不到圆角前的 L：${d}`);
  const startX = Number(beforeQuad![1]);
  const radius = 100 - startX;
  assert.ok(radius > 0 && radius <= ROUND_RADIUS, `圆角半径异常：${radius}`);
  // 半径不能超过两段长度的一半
  assert.ok(radius <= 40, `半径超过段长一半：${radius}`);
  assert.ok(!d.includes('NaN'), 'd 里出现了 NaN');
});

// ==================== ③ 两个拐角 ====================

test('③ 两个拐点（Z 形）→ 2 个 Q', () => {
  const d = buildRoundedPath([
    { x: 0, y: 0 },
    { x: 50, y: 0 },
    { x: 50, y: 60 },
    { x: 120, y: 60 },
  ]);
  assert.equal(countQuads(d), 2, `应有 2 个圆角，实际 d=${d}`);
});

// ==================== ④ 短段自动缩小半径 ====================

test('④ 段长 < 16px：半径自动减半，绝不超出段长', () => {
  const d = buildRoundedPath([
    { x: 0, y: 0 },
    { x: 10, y: 0 }, // 入段 10px
    { x: 10, y: 10 }, // 出段 10px
    { x: 100, y: 10 },
  ]);
  assert.equal(countQuads(d), 2);
  // 第一个拐点 (10,0)：r 必须 ≤ 10/2 = 5
  const startX = numbers(d)[2];
  const radius = 10 - startX;
  assert.ok(radius > 0 && radius <= 5, `短段半径应 ≤ 半段长：${radius}`);
  // 关键不变量：圆角起点必须落在两段之内，不能出现「回头钩」
  assert.ok(startX >= 0 && startX <= 10, `圆角起点越界：${startX}`);
});

// ==================== ⑤ 退化输入 ====================

test('⑤ 起终点相同 / 单点 → 不产生非法 path', () => {
  const same = buildRoundedPath([
    { x: 40, y: 40 },
    { x: 40, y: 40 },
  ]);
  assert.ok(same.startsWith('M 40 40'), `应以 M 开头：${same}`);
  assert.ok(!same.includes('NaN') && !same.includes('Infinity'), `非法 path：${same}`);
  assert.equal(countQuads(same), 0);

  assert.equal(buildRoundedPath([]), '');
  assert.equal(buildRoundedPath([{ x: 7, y: 9 }]), 'M 7 9 L 7 9');
});

// ==================== ⑥ 接入 `edgePath` 的两条真实路径形态 ====================

test('⑥ 真实折线（右向 / 回流绕行）都能圆化，且点数与线段数不变', () => {
  // 右向：M from L mid,fromY L mid,toY L to（1 个拐点 ×2）
  const normal = roundedEdgePath('M 100 50 L 165 50 L 165 120 L 230 120');
  assert.equal(countQuads(normal), 2);
  // 回流：下绕一圈（5 段 → 4 个拐点）
  const detour = roundedEdgePath('M 300 50 L 320 50 L 320 170 L 200 170 L 200 120 L 180 120');
  assert.equal(countQuads(detour), 4, detour);
  assert.ok(!detour.includes('NaN'));
  // 少于两个点时原样返回（不把脏数据变得更脏）
  assert.equal(roundedEdgePath('M 10 10'), 'M 10 10');
});

// ==================== ⑦ 标签宽度估算 ====================

test('⑦ 边标签宽度：中文按整字宽、ASCII 按 0.55 字宽，且向上取整', () => {
  assert.equal(labelTextWidth('是'), 11); // 1 字 × 10.5 → ceil 11
  assert.equal(labelTextWidth('其它情况'), 42); // 4 字 × 10.5
  // 同样字数（8 个字符）下，ASCII 应明显窄于中文
  const ascii = labelTextWidth('abcdefgh');
  const cjk = labelTextWidth('其它情况其它情况');
  assert.ok(ascii > 0 && ascii < cjk, `ASCII 应比同字数中文窄：ascii=${ascii} cjk=${cjk}`);
  assert.equal(labelTextWidth(''), 0);
});
