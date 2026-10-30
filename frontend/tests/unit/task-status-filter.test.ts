/**
 * `status-filter.ts` 单测（T9-B）：状态勾选筛选的三支纯逻辑。
 *
 * 卡面验收里「勾选即筛、可多选、清空回全量」这些行为是**交互**，e2e 能验外观，
 * 但数组顺序、空数组不传参这类契约细节 e2e 看不见；而且这两条一旦写错，
 * 现象是「后端不报错但筛不中」（空串被当成筛一个空状态），排查成本极高。
 * 所以把逻辑抽成纯函数在这里钉死（本仓惯例，见 `tree-layout.test.ts`）。
 *
 * 运行：`pnpm test:unit`
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  STATUSES_SEPARATOR,
  clearStatusList,
  hasStatusFilter,
  statusesParam,
  toggleStatusIn,
} from '../../src/views/business/task/components/status-filter.ts';

test('toggleStatus 添加：[] → ["TODO"]', () => {
  assert.deepEqual(toggleStatusIn([], 'TODO'), ['TODO']);
});

test('toggleStatus 再点同一个：["TODO"] → []', () => {
  assert.deepEqual(toggleStatusIn(['TODO'], 'TODO'), []);
});

test('toggleStatus 多选：["TODO","DOING"] 顺序保持（先点的在前）', () => {
  const afterFirst = toggleStatusIn([], 'TODO');
  const afterSecond = toggleStatusIn(afterFirst, 'DOING');
  assert.deepEqual(afterSecond, ['TODO', 'DOING']);
  // 取消中间那个，剩下的顺序不能乱
  assert.deepEqual(toggleStatusIn(afterSecond, 'TODO'), ['DOING']);
  // 三选后再取消中间项
  const three = toggleStatusIn(afterSecond, 'REVIEW');
  assert.deepEqual(three, ['TODO', 'DOING', 'REVIEW']);
  assert.deepEqual(toggleStatusIn(three, 'DOING'), ['TODO', 'REVIEW']);
});

test('clearStatuses：任意 → []', () => {
  assert.deepEqual(clearStatusList(), []);
  // 每次都是**新**数组（不能返回同一个共享引用，否则多个页面实例会串）
  assert.notEqual(clearStatusList(), clearStatusList());
});

test('请求参数：statuses 数组 → 逗号串；空数组 → undefined（= 不筛）', () => {
  assert.equal(STATUSES_SEPARATOR, ',');
  assert.equal(statusesParam(['TODO', 'DOING']), 'TODO,DOING');
  assert.equal(statusesParam(['DOING']), 'DOING');
  assert.equal(statusesParam([]), undefined);
  // 关键：空数组必须给 undefined，不能给 ''（空串会被后端当成筛一个空状态）
  assert.notEqual(statusesParam([]) as unknown, '');
});

test('待勾选态判定：空数组不算筛选，有勾选才算', () => {
  assert.equal(hasStatusFilter([]), false);
  assert.equal(hasStatusFilter(['TODO']), true);
});

test('纯函数：不修改入参（Vue reactive 数组被原地改动会绕过赋值、触发不了依赖）', () => {
  const origin: string[] = ['TODO'];
  const next = toggleStatusIn(origin, 'DOING');
  assert.deepEqual(origin, ['TODO']);
  assert.notEqual(next, origin);
});
