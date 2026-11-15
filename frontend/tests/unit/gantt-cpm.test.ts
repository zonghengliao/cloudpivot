/**
 * 甘特关键路径（CPM）单测（P3-14）：三链定长、浮动、关键边、汇总滚up、脏数据兜底。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * # 为什么这几个点非测不可
 *
 * 关键路径是「算错也照样画得出来」的功能 —— 红色一画上去，用户看不出对错。
 * 所以断言全部落在**数字**上（ES / EF / 浮动 / 项目工期），不靠截图：
 *
 * 1. **3 条依赖链高亮最长那条**（卡面验收）：最长链 = 顶到项目完成日的那条，浮动 0；
 *    短的链必须浮动 > 0 —— 若「所有链都标红」，功能等于没做；
 * 2. **链上有空档时，空档前面那个任务不该关键**：浮动是「还能滑几天」，不是「在不在链上」；
 * 3. **排期自相矛盾**（后继计划开始早于前置结束）要如实标 `conflict`，不能静默改数字；
 * 4. **依赖环**：算不出来就 `null`，绝不假报 0（0 会被画成关键路径，等于撒谎）；
 * 5. **父任务（汇总条）不是节点**，只做 rollup —— 否则工期算两遍。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { computeCpm, cpmEdgeKey, floatLabel, type CpmTaskInput } from '../../src/views/business/task/gantt-cpm.ts';

/** 造任务（只带算法要的字段） */
function task(id: string, start: string | null, due: string | null, parentId: string | null = null): CpmTaskInput {
  return { id, start_date: start, due_date: due, parent_id: parentId };
}

/** 依赖边（默认 FS、滞后 0） */
function dep(from: string, to: string, lagDays = 0): { from: string; to: string; lag_days: number } {
  return { from, to, lag_days: lagDays };
}

test('3 条依赖链：只标红最长那条（顶到项目完成日的那条），短链有浮动', () => {
  // 基准日 2026-03-02
  // 链 A（短，4 天）：A1 03-02~03-03(2d) → A2 03-04~03-05(2d)
  // 链 B（最长，7 天）：B1 03-02~03-04(3d) → B2 03-05~03-08(4d)
  // 链 C（中，5 天）：C1 03-02(1d) → C2 03-03~03-05(3d) → C3 03-06(1d)
  const tasks = [
    task('A1', '2026-03-02', '2026-03-03'),
    task('A2', '2026-03-04', '2026-03-05'),
    task('B1', '2026-03-02', '2026-03-04'),
    task('B2', '2026-03-05', '2026-03-08'),
    task('C1', '2026-03-02', '2026-03-02'),
    task('C2', '2026-03-03', '2026-03-05'),
    task('C3', '2026-03-06', '2026-03-06'),
  ];
  const deps = [
    dep('A1', 'A2'),
    dep('B1', 'B2'),
    dep('C1', 'C2'),
    dep('C2', 'C3'),
  ];
  const cpm = computeCpm({ tasks, deps });

  assert.equal(cpm.origin, '2026-03-02');
  assert.equal(cpm.projectDays, 7, '项目工期 = 最长链的总天数');
  assert.equal(cpm.skippedIds.length, 0);
  assert.equal(cpm.cyclicIds.length, 0);

  // 关键任务 = 链 B 的两个任务，且**只有**它们
  const critical = [...cpm.byId.values()]
    .filter((node) => node.critical)
    .map((node) => node.id)
    .sort();
  assert.deepEqual(critical, ['B1', 'B2']);

  // 链 B：紧排、浮动 0
  assert.deepEqual(
    [cpm.byId.get('B1')!.earliestStart, cpm.byId.get('B1')!.earliestFinish, cpm.byId.get('B1')!.totalFloat],
    [0, 3, 0],
  );
  assert.deepEqual(
    [cpm.byId.get('B2')!.earliestStart, cpm.byId.get('B2')!.earliestFinish, cpm.byId.get('B2')!.totalFloat],
    [3, 7, 0],
  );

  // 链 A：末端离项目完成日还有 3 天余量
  assert.deepEqual(
    [cpm.byId.get('A2')!.earliestFinish, cpm.byId.get('A2')!.latestStart, cpm.byId.get('A2')!.totalFloat],
    [4, 5, 3],
  );
  assert.equal(cpm.byId.get('A1')!.totalFloat, 3);

  // 链 C：紧排（C1 day0、C2 day1、C3 day4），整条链比项目完成日早 2 天 → 浮动都是 2
  assert.deepEqual(
    [cpm.byId.get('C3')!.earliestFinish, cpm.byId.get('C3')!.totalFloat, cpm.byId.get('C2')!.totalFloat, cpm.byId.get('C1')!.totalFloat],
    [5, 2, 2, 2],
  );

  // 关键**边**：只有 B1->B2 在顶（A/C 的边两端不都关键）
  assert.deepEqual([...cpm.criticalEdges], [cpmEdgeKey('B1', 'B2')]);
  assert.equal(cpm.criticalEdges.has(cpmEdgeKey('A1', 'A2')), false);
});

test('链上有空档：空档后的任务关键，空档前的任务不关键（浮动 ≠ 在链上）', () => {
  // P1 03-02(1d) 结束于 day1，P2 计划 day3 才开始 —— 中间空 2 天
  const cpm = computeCpm({
    tasks: [
      task('P1', '2026-03-02', '2026-03-02'),
      task('P2', '2026-03-05', '2026-03-06'),
      task('Q1', '2026-03-02', '2026-03-05'),
    ],
    deps: [dep('P1', 'P2')],
  });

  assert.equal(cpm.projectDays, 5);
  // P2 顶到项目完成日 → 关键
  assert.deepEqual(
    [cpm.byId.get('P2')!.earliestStart, cpm.byId.get('P2')!.totalFloat, cpm.byId.get('P2')!.critical],
    [3, 0, true],
  );
  // P1 可以整体后滑 2 天（填掉空档）而不影响项目完成日 → 不关键
  assert.deepEqual(
    [cpm.byId.get('P1')!.earliestFinish, cpm.byId.get('P1')!.latestStart, cpm.byId.get('P1')!.totalFloat],
    [1, 2, 2],
  );
  assert.equal(cpm.byId.get('P1')!.critical, false);
  // Q1 与 P2 同一天完成？Q1 4 天 → EF 4，余量 1
  assert.equal(cpm.byId.get('Q1')!.totalFloat, 1);
  // 空档让这条边不再「在顶」：ES_P2(3) ≠ EF_P1(1) + 0
  assert.equal(cpm.criticalEdges.has(cpmEdgeKey('P1', 'P2')), false);
});

test('排期自相矛盾：后继计划开始早于前置结束 → 标 conflict，且按依赖顶住算', () => {
  const cpm = computeCpm({
    tasks: [
      task('S1', '2026-03-02', '2026-03-05'), // 4 天，EF = 4
      task('S2', '2026-03-03', '2026-03-04'), // 计划 day1 开始，但前置 day4 才结束
    ],
    deps: [dep('S1', 'S2')],
  });

  const succ = cpm.byId.get('S2')!;
  assert.equal(succ.plannedStart, 1, '计划开始如实保留');
  assert.equal(succ.earliestStart, 4, '最早开始被前置顶到 day4');
  assert.equal(succ.conflict, true, '矛盾必须说出来');
  assert.equal(succ.duration, 2);
  assert.equal(cpm.projectDays, 6);
  // 顶住之后两者都无余量（这是「数据已互相打架」的诚实结论）
  assert.equal(cpm.byId.get('S1')!.critical, true);
  assert.equal(succ.critical, true);
  assert.equal(cpm.criticalEdges.has(cpmEdgeKey('S1', 'S2')), true);
});

test('依赖环：不假报关键（浮动 null），环里的节点单独列出', () => {
  const cpm = computeCpm({
    tasks: [
      task('X', '2026-03-02', '2026-03-03'),
      task('Y', '2026-03-04', '2026-03-05'),
      task('Z', '2026-03-02', '2026-03-02'),
    ],
    deps: [dep('X', 'Y'), dep('Y', 'X')],
  });

  assert.deepEqual([...cpm.cyclicIds].sort(), ['X', 'Y']);
  for (const id of ['X', 'Y']) {
    const node = cpm.byId.get(id)!;
    assert.equal(node.inCycle, true);
    assert.equal(node.totalFloat, null, '环里算不出浮动，不能填 0');
    assert.equal(node.critical, false, '算不出来就不许画成关键路径');
    assert.equal(node.latestStart, null);
  }
  // 环外的任务照常算
  assert.equal(cpm.byId.get('Z')!.totalFloat !== null, true);
  // 环上的边不进关键边集合
  assert.equal(cpm.criticalEdges.size, 0);
  assert.equal(floatLabel(cpm.byId.get('X')!.totalFloat), '浮动 —（依赖成环）');
});

test('父任务（汇总条）不是节点：只 rollup「有子任务关键就标红」，工期不重复计算', () => {
  const tasks = [
    task('P', '2026-03-02', '2026-03-06'), // 汇总条（有条目更早的自身日期也不参与计算）
    task('K1', '2026-03-02', '2026-03-03', 'P'),
    task('K2', '2026-03-04', '2026-03-06', 'P'),
    task('L', '2026-03-02', '2026-03-03'),
  ];
  const cpm = computeCpm({ tasks, deps: [dep('K1', 'K2')] });

  assert.equal(cpm.byId.has('P'), false, '汇总条不许当节点（否则工期算两遍）');
  assert.equal(cpm.byId.has('K1'), true);
  assert.equal(cpm.projectDays, 5, '项目工期只由叶子任务决定');

  const summary = cpm.summaryById.get('P');
  assert.ok(summary, '汇总条要有 rollup');
  assert.equal(summary!.leafCount, 2);
  assert.equal(summary!.criticalCount, 2);
  assert.equal(summary!.critical, true);
  assert.equal(summary!.minFloat, 0);

  // 独立任务 L 有 3 天余量 → 不关键
  assert.equal(cpm.byId.get('L')!.totalFloat, 3);
});

test('汇总条自身不关键时 rollup 不标红（子任务都有余量）', () => {
  const tasks = [
    task('P', null, null),
    task('K1', '2026-03-02', '2026-03-02', 'P'),
    task('R', '2026-03-02', '2026-03-09'), // 9 天，把项目完成日顶到 day9
  ];
  const cpm = computeCpm({ tasks, deps: [] });
  const summary = cpm.summaryById.get('P')!;
  assert.equal(summary.critical, false);
  assert.equal(summary.criticalCount, 0);
  assert.equal(summary.minFloat, 7);
  assert.equal(cpm.byId.get('K1')!.totalFloat, 7);
  assert.equal(cpm.byId.get('R')!.critical, true);
});

test('依赖连到父任务：展开成它的每个叶子（「等父任务的活全干完」）', () => {
  const tasks = [
    task('P', null, null),
    task('K1', '2026-03-02', '2026-03-03', 'P'), // EF 2
    task('K2', '2026-03-02', '2026-03-06', 'P'), // EF 5（更晚）
    task('M', '2026-03-07', '2026-03-08'), // 计划 day5 开始
  ];
  const cpm = computeCpm({ tasks, deps: [dep('P', 'M')] });

  // M 的最早开始受两个叶子同时约束，取更晚的那个
  assert.equal(cpm.byId.get('M')!.earliestStart, 5);
  assert.equal(cpm.byId.get('M')!.conflict, false);
  assert.equal(cpm.projectDays, 7);
  // 原始边仍用 `P->M` 作键（模板里 link.key 就是这个形状）
  assert.equal(cpm.criticalEdges.has(cpmEdgeKey('P', 'M')), true);
});

test('父子互换的假依赖 / 自环：直接忽略，不产生节点之外的边', () => {
  const cpm = computeCpm({
    tasks: [task('A', '2026-03-02', '2026-03-03'), task('B', '2026-03-04', '2026-03-05')],
    deps: [dep('A', 'A'), dep('A', 'B'), dep('A', 'B')],
  });
  assert.equal(cpm.byId.get('A')!.earliestStart, 0);
  // A 03-02~03-03 = day0 起 2 天 → EF 2；B 计划 day2 开始，重复边不该把开始日往后推
  assert.equal(cpm.byId.get('B')!.earliestStart, 2, '重复边不该把开始日往后推');
  assert.equal(cpm.criticalEdges.size, 1);
});

test('滞后天数计入（ES_succ ≥ EF_pred + lag）', () => {
  const cpm = computeCpm({
    tasks: [
      task('A', '2026-03-02', '2026-03-03'), // EF 2
      task('B', '2026-03-05', '2026-03-06'), // 计划 day3，恰好 = 2 + lag(1)
    ],
    deps: [dep('A', 'B', 1)],
  });
  assert.equal(cpm.byId.get('B')!.earliestStart, 3);
  assert.equal(cpm.byId.get('B')!.conflict, false);
  assert.equal(cpm.byId.get('A')!.totalFloat, 0);
  assert.equal(cpm.criticalEdges.has(cpmEdgeKey('A', 'B')), true);
});

test('缺日期 / 单边日期：缺的跳过并上报，单边的按单日算', () => {
  const cpm = computeCpm({
    tasks: [task('N', null, null), task('ONLY_DUE', null, '2026-03-03'), task('OK', '2026-03-02', '2026-03-02')],
    deps: [],
  });
  assert.deepEqual(cpm.skippedIds, ['N']);
  const onlyDue = cpm.byId.get('ONLY_DUE')!;
  assert.equal(onlyDue.plannedStart, 1);
  assert.equal(onlyDue.duration, 1, '只有截止日 → 按 1 天算（与条形口径一致）');
  assert.equal(cpm.origin, '2026-03-02');
});

test('空数据 / 全缺日期：不炸，返回空结果', () => {
  const empty = computeCpm({ tasks: [], deps: [] });
  assert.equal(empty.origin, null);
  assert.equal(empty.byId.size, 0);
  assert.equal(empty.projectDays, 0);

  const noDates = computeCpm({ tasks: [task('X', null, null)], deps: [] });
  assert.equal(noDates.origin, null);
  assert.deepEqual(noDates.skippedIds, ['X']);
});

test('浮动文案：0 / 正数 / 算不出来三种说法', () => {
  assert.equal(floatLabel(0), '浮动 0 天（关键路径）');
  assert.equal(floatLabel(-1), '浮动 0 天（关键路径）');
  assert.equal(floatLabel(3), '浮动 3 天');
  assert.equal(floatLabel(null), '浮动 —（依赖成环）');
});

test('大图性能：1000 条任务的链 + 平行支路，毫秒级算完', () => {
  const tasks: CpmTaskInput[] = [];
  const deps: Array<{ from: string; to: string; lag_days: number }> = [];
  for (let index = 0; index < 1000; index += 1) {
    const day = 1 + index; // 2026-03-01 起每天一条
    const date = new Date(Date.UTC(2026, 2, day)).toISOString().slice(0, 10);
    tasks.push(task(`T${index}`, date, date));
    if (index > 0) {
      deps.push(dep(`T${index - 1}`, `T${index}`));
    }
  }
  const started = Date.now();
  const cpm = computeCpm({ tasks, deps });
  const elapsed = Date.now() - started;
  assert.equal(cpm.projectDays, 1000);
  assert.equal([...cpm.byId.values()].filter((node) => node.critical).length, 1000);
  assert.ok(elapsed < 500, `1000 条任务应该远快于 500ms，实测 ${elapsed}ms`);
});
