/**
 * 甘特依赖连线单测（P3-13）：几何走法 + 依赖类型文案 + 悬停浮层。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * # 为什么测这几个点
 *
 * 依赖线在真机上是「看得见就对了」的东西，但**走法**有三个会静默出错的边界：
 *
 * 1. **间隙充足**：竖段要贴前置那侧（`x1 + STUB`），不能飘到两行正中（看起来像连错了人）；
 * 2. **间隙很窄**：竖段落在间隙中点，`d` 里不能出现「出去又回来」的回头段；
 * 3. **后继排在前置左边**：必须绕行到两行之间的空带，且**所有 x ≥ 左栏宽 + 2**
 *    —— 越过这条线就会把依赖线画到左侧任务名上（截图里像是任务名被划掉了）。
 *
 * 以及一条**红线**：命中区（透明加粗路径）不能盖住箭头尖 ——
 * 箭头尖正好落在后继条形左边缘，那里是拖拽改期的左手柄，盖住就「能悬停但拖不动」。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  buildFsLink,
  depReleased,
  depTypeLabel,
  linkTipLines,
  type BarRect,
  type LinkLayout,
} from '../../src/views/business/task/gantt-links.ts';

const LEFT_W = 280;
const BAR_TOP = 8;
const BAR_H = 18;
const ROW_H = 34;
const HEAD_H = 46;
/** 与 gantt-links 内部常量同值（命中区宽度是导出常量，这里只断言它「不覆盖最后一段」） */
const STUB = 12;

/** 第 index 行的行顶（与 gantt.vue 的 rowTop 同式） */
function rowTop(index: number): number {
  return HEAD_H + index * ROW_H;
}

function layout(predRow: number, succRow: number): LinkLayout {
  return {
    leftWidth: LEFT_W,
    predRowTop: rowTop(predRow),
    succRowTop: rowTop(succRow),
    barTopOffset: BAR_TOP,
    barHeight: BAR_H,
    maxX: 4000,
  };
}

/** 条形中点 y（前置 y1 / 后继 y2） */
function barMidY(row: number): number {
  return rowTop(row) + BAR_TOP + BAR_H / 2;
}

test('间隙充足：右出 STUB → 竖段 → 水平进入后继左边缘', () => {
  const pred: BarRect = { x: 300, width: 60 };
  const succ: BarRect = { x: 500, width: 60 };
  const geom = buildFsLink(pred, succ, layout(0, 1));
  const y1 = barMidY(0);
  const y2 = barMidY(1);
  const x1 = pred.x + pred.width;

  assert.deepEqual(geom.points, [
    { x: x1, y: y1 },
    { x: x1 + STUB, y: y1 },
    { x: x1 + STUB, y: y2 },
    { x: 500, y: y2 },
  ]);
  // 箭头尖 = 后继条形左边缘中点（FS 的「开始」一侧）
  assert.deepEqual(geom.tip, { x: 500, y: y2 });
  // 路径首尾：从前置右端中点出发，到箭头尖结束
  assert.ok(geom.d.startsWith(`M ${x1},${y1}`), `d 起点不对：${geom.d}`);
  assert.ok(geom.d.endsWith(`L 500,${y2}`), `d 终点不对：${geom.d}`);
  // 竖段那一侧有圆角（避免直角硬拐）
  assert.ok(geom.d.includes(`Q ${x1 + STUB},${y1}`), `d 里没有竖段圆角：${geom.d}`);
});

test('间隙很窄：竖段落在间隙中点（不前冲也不回头）', () => {
  const pred: BarRect = { x: 300, width: 60 };
  const succ: BarRect = { x: 370, width: 60 };
  const geom = buildFsLink(pred, succ, layout(0, 1));
  const y1 = barMidY(0);
  const y2 = barMidY(1);
  const midX = (pred.x + pred.width + succ.x) / 2;

  assert.deepEqual(geom.points, [
    { x: 360, y: y1 },
    { x: midX, y: y1 },
    { x: midX, y: y2 },
    { x: 370, y: y2 },
  ]);
  // 唯一的竖段：x 单调不减（没有回头段）
  for (let index = 1; index < geom.points.length; index += 1) {
    assert.ok(geom.points[index]!.x >= geom.points[index - 1]!.x, '窄间隙不该出现回头段');
  }
});

test('后继排在前置左边：绕行两行之间的空带，且不越过左侧任务名栏', () => {
  const pred: BarRect = { x: 400, width: 80 };
  const succ: BarRect = { x: 320, width: 40 };
  const geom = buildFsLink(pred, succ, layout(0, 1));
  const y1 = barMidY(0);
  const y2 = barMidY(1);

  // 6 个顶点：出 → 下到空带 → 左折 → 再下 → 进入后继左边缘
  assert.equal(geom.points.length, 6, `绕行走法应该是 6 个顶点：${JSON.stringify(geom.points)}`);
  assert.deepEqual(geom.points[2], { x: 480 + STUB, y: (y1 + y2) / 2 });
  // 红线：任何一段都不许伸进名字栏（LEFT_W 左侧）
  for (const point of geom.points) {
    assert.ok(point.x >= LEFT_W + 2, `线段伸进了任务名栏：${JSON.stringify(point)}`);
  }
  assert.deepEqual(geom.tip, { x: 320, y: y2 });
});

test('条形被时间窗裁到左边界：起点钳在名字栏右侧，不画到任务名上', () => {
  const pred: BarRect = { x: 200, width: 20 }; // 被窗口裁过（左沿 < LEFT_W）
  const succ: BarRect = { x: 600, width: 60 };
  const geom = buildFsLink(pred, succ, layout(0, 1));
  assert.equal(geom.points[0]!.x, LEFT_W + 2);
});

test('命中路径去掉最后一段：不盖住箭头尖（后继条形的拖拽左手柄）', () => {
  const pred: BarRect = { x: 300, width: 60 };
  const succ: BarRect = { x: 500, width: 60 };
  const geom = buildFsLink(pred, succ, layout(0, 2));
  const y2 = barMidY(2);

  // 命中路径的终点停在竖段下端，而不是箭头尖
  assert.ok(geom.dHit.endsWith(`L ${300 + 60 + STUB},${y2}`), `命中路径终点不对：${geom.dHit}`);
  assert.ok(!geom.dHit.endsWith(`L 500,${y2}`), '命中路径不该覆盖箭头尖');

  // 单段情形（同一条行）退化成一个点：命中路径等于可见路径
  const flat = buildFsLink({ x: 300, width: 60 }, { x: 900, width: 60 }, {
    ...layout(1, 1),
    maxX: 4000,
  });
  assert.equal(flat.dHit, flat.d, '同一条行时命中路径与可见路径相同');
});

test('依赖类型中文名：FS 是本版唯一真值，未知类型原样回显', () => {
  assert.equal(depTypeLabel('FS'), '完成-开始');
  assert.equal(depTypeLabel('fs'), '完成-开始');
  assert.equal(depTypeLabel(undefined), '完成-开始');
  assert.equal(depTypeLabel(null), '完成-开始');
  assert.equal(depTypeLabel('SS'), '开始-开始');
  assert.equal(depTypeLabel('FF'), '完成-完成');
  assert.equal(depTypeLabel('SF'), '开始-完成');
  // 后端将来加了新类型：不吞数据，原样显示
  assert.equal(depTypeLabel('XY'), 'XY');
});

test('依赖是否已解除：与后端「DONE / CANCELLED 视为不再阻塞」同口径', () => {
  assert.equal(depReleased('DONE'), true);
  assert.equal(depReleased('CANCELLED'), true);
  assert.equal(depReleased('DOING'), false);
  assert.equal(depReleased('REVIEW'), false);
  assert.equal(depReleased('TODO'), false);
  assert.equal(depReleased(null), false);
});

test('悬停浮层：第一行就是依赖类型，滞后天数只在 > 0 时出现', () => {
  const base = {
    depType: 'FS',
    pred: { taskNo: 'TSK-1', title: '前置任务' },
    succ: { taskNo: 'TSK-2', title: '后继任务' },
    released: false,
  };

  const lines = linkTipLines({ ...base, lagDays: 0 });
  assert.equal(lines[0], '依赖类型：完成-开始（FS）');
  assert.equal(lines[1], '前置 TSK-1 前置任务');
  assert.equal(lines[2], '后继 TSK-2 后继任务');
  assert.equal(lines[3], '前置未完成（后继被阻塞）');
  assert.equal(lines.length, 4);

  // 后端没下发 dep_type / lag_days（老接口）：退化成 FS / 0 天，不显示「滞后 undefined」
  const legacy = linkTipLines({ ...base, depType: undefined, lagDays: undefined } as never);
  assert.equal(legacy[0], '依赖类型：完成-开始（FS）');
  assert.equal(legacy[3], '前置未完成（后继被阻塞）');

  const lagged = linkTipLines({ ...base, lagDays: 3, released: true });
  assert.equal(lagged[3], '滞后 3 天 · 前置已完成 / 已取消（依赖已解除）');
});
