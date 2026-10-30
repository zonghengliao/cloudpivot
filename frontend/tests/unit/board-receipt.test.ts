/**
 * `board-receipt.ts` 单测（`node --test`，零新依赖）。
 *
 * 覆盖的核心契约：**服务端已确认（200 + code 0）之后，回执侧无论缺字段还是类型漂移，
 * 都不许抛异常** —— 因为 `moveTask` 用它来区分「请求失败回弹」与「回执处理失败」，
 * 一旦这里抛出去，就会走回弹分支，出现「服务端成功、页面把卡片退回源列」的脏状态
 * （2026-10-30 12:20 流程3 那次偶发红就是这个形状，见 frontend/e2e/README.md「已知前提」）。
 *
 * 运行：`node --test tests/unit/`
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  applyStatusReceipt,
  syncParentProgress,
  type ReceiptCard,
} from '../../src/views/business/task/components/board-receipt.ts';

/** 造一张卡 */
const card = (id: string, status: string, progress = 0): ReceiptCard => ({ id, status, progress });

test('回执齐全：卡片状态/进度按回执校正，父任务进度同步到板上', () => {
  const doing = [card('t1', 'DOING', 0)];
  const todo = [card('p1', 'TODO', 40)];
  const out = applyStatusReceipt({
    targetTasks: doing,
    taskId: 't1',
    receipt: { status: 'DOING', progress: 10, parent_progress: [{ id: 'p1', progress: 100 }] },
    fallbackStatus: 'DOING',
    boardTasks: [todo, doing],
  });
  assert.equal(doing[0]?.status, 'DOING');
  assert.equal(doing[0]?.progress, 10);
  assert.equal(todo[0]?.progress, 100, '父任务进度应被同步');
  assert.equal(out.applied, true);
  assert.equal(out.parentsSynced, 1);
  assert.equal(out.error, undefined);
});

test('回执缺 parent_progress（undefined）：不抛异常，卡片留在目标列', () => {
  const doing = [card('t1', 'DOING', 0)];
  const out = applyStatusReceipt({
    targetTasks: doing,
    taskId: 't1',
    receipt: { status: 'DOING', progress: 10 },
    fallbackStatus: 'DOING',
    boardTasks: [doing],
  });
  assert.equal(out.error, undefined, '缺 parent_progress 不许炸');
  assert.equal(doing[0]?.status, 'DOING');
  assert.equal(doing[0]?.progress, 10);
});

test('回执 parent_progress 为 null：不抛异常，卡片留在目标列', () => {
  const doing = [card('t1', 'DOING', 0)];
  const out = applyStatusReceipt({
    targetTasks: doing,
    taskId: 't1',
    // 后端实测是 []，但这里显式覆盖「类型漂移成 null」的防御
    receipt: { status: 'DOING', progress: 10, parent_progress: null as unknown as undefined },
    fallbackStatus: 'DOING',
    boardTasks: [doing],
  });
  assert.equal(out.error, undefined);
  assert.equal(doing[0]?.progress, 10);
});

test('回执整个为 null（body.data 为空）：用 fallbackStatus 兜底，不抛也不回滚', () => {
  const doing = [card('t1', 'DOING', 0)];
  const out = applyStatusReceipt({
    targetTasks: doing,
    taskId: 't1',
    receipt: null,
    fallbackStatus: 'DOING',
    boardTasks: [doing],
  });
  assert.equal(out.error, undefined, '空回执不许炸（否则页面会回滚到源列）');
  assert.equal(doing[0]?.status, 'DOING', '状态用拖拽目标列兜底');
});

test('回执缺 status：用 fallbackStatus（拖拽目标列）兜底', () => {
  const doing = [card('t1', 'DOING', 0)];
  const out = applyStatusReceipt({
    targetTasks: doing,
    taskId: 't1',
    receipt: { progress: 10, parent_progress: [] },
    fallbackStatus: 'DOING',
    boardTasks: [doing],
  });
  assert.equal(out.error, undefined);
  assert.equal(doing[0]?.status, 'DOING');
  assert.equal(doing[0]?.progress, 10);
});

test('parent_progress 是对象而非数组：安全跳过，不影响卡片校正', () => {
  const doing = [card('t1', 'DOING', 0)];
  const out = applyStatusReceipt({
    targetTasks: doing,
    taskId: 't1',
    receipt: { status: 'DOING', progress: 10, parent_progress: { id: 'p1' } as unknown as [] },
    fallbackStatus: 'DOING',
    boardTasks: [doing],
  });
  assert.equal(out.error, undefined);
  assert.equal(out.parentsSynced, 0);
  assert.equal(doing[0]?.progress, 10);
});

test('parent_progress 元素缺字段：跳过坏元素，继续处理后面的（不整体丢弃）', () => {
  const board = [[card('p1', 'TODO', 0)], [card('p2', 'TODO', 0)]];
  const synced = syncParentProgress(
    [
      { id: undefined, progress: 99 } as unknown as { id: string; progress: number },
      { id: 'p1', progress: 'x' as unknown as number },
      { id: 'p2', progress: 100 },
    ],
    board,
  );
  assert.equal(synced, 1, '只有合法的那一条被同步');
  assert.equal(board[1]?.[0]?.progress, 100);
  assert.equal(board[0]?.[0]?.progress, 0, '坏元素不该被写坏');
});

test('目标列里找不到该卡（回执早于列表刷新到）：applied=false 且不抛', () => {
  const out = applyStatusReceipt({
    targetTasks: [card('other', 'DOING', 0)],
    taskId: 't1',
    receipt: { status: 'DOING', progress: 10, parent_progress: [] },
    fallbackStatus: 'DOING',
    boardTasks: [],
  });
  assert.equal(out.applied, false);
  assert.equal(out.error, undefined);
});

test('progress 非数字（后端类型漂移成字符串）：不写坏卡片进度', () => {
  const doing = [card('t1', 'DOING', 0)];
  const out = applyStatusReceipt({
    targetTasks: doing,
    taskId: 't1',
    receipt: { status: 'DOING', progress: '10' as unknown as number, parent_progress: [] },
    fallbackStatus: 'DOING',
    boardTasks: [doing],
  });
  assert.equal(out.error, undefined);
  assert.equal(doing[0]?.progress, 0, '非法进度保留原值，不写 NaN/字符串');
});

test('boardTasks 缺席（不传）也不炸', () => {
  const doing = [card('t1', 'DOING', 0)];
  const out = applyStatusReceipt({
    targetTasks: doing,
    taskId: 't1',
    receipt: { status: 'DOING', progress: 10, parent_progress: [{ id: 'p1', progress: 5 }] },
    fallbackStatus: 'DOING',
  });
  assert.equal(out.error, undefined);
  assert.equal(out.parentsSynced, 0);
});
