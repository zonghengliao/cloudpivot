/**
 * 登录后「你有 N 条待处理」汇总弹窗的口径单测。
 *
 * 被测：`src/utils/login-unread.ts`（四类取数汇总 / 合计 / 「本次登录只弹一次」标记）。
 * `LoginUnreadDialog.vue` 只是把 loader 换成真接口 + 把结果铺进 `t-dialog`，判断逻辑全在这里。
 *
 * 运行：`pnpm test:unit`
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  clearLoginUnreadShown,
  collectUnread,
  emptyCounts,
  isLoginUnreadShown,
  LOGIN_UNREAD_SHOWN_KEY,
  markLoginUnreadShown,
  permittedUnreadKeys,
  pickUnreadLoaders,
  summarizeUnread,
  totalUnread,
  UNREAD_SOURCES,
  type UnreadLoaders,
} from '../../src/utils/login-unread';

/** 每个用例前后重置「已弹过」标记，避免用例互相污染 */
function resetShown(): void {
  clearLoginUnreadShown();
}

test('四类汇总：条数与合计正确，顺序与跳转路径固定', async () => {
  resetShown();
  const counts = { notice: 3, announcement: 2, task: 1, approval: 1 };
  const rows = summarizeUnread(counts);
  assert.equal(rows.length, 4, '四个类别恒定 4 行（0 条的行由界面置灰）');
  assert.deepEqual(
    rows.map((row) => [row.label, row.count]),
    [
      ['站内信', 3],
      ['公告', 2],
      ['工作任务', 1],
      ['审批待办', 1],
    ],
  );
  assert.deepEqual(
    rows.map((row) => row.path),
    ['/business/notice/message', '/announcement', '/task/list', '/flow/task'],
  );
  assert.equal(totalUnread(counts), 7);
});

test('collectUnread 并行取数：全部成功时逐类落到对应键', async () => {
  resetShown();
  const loaders: UnreadLoaders = {
    notice: async () => 9,
    announcement: async () => 0,
    task: async () => 2,
    approval: async () => 8,
  };
  const counts = await collectUnread(loaders);
  assert.deepEqual(counts, { notice: 9, announcement: 0, task: 2, approval: 8 });
  assert.equal(totalUnread(counts), 19);
});

test('collectUnread 单类失败按 0 计，不影响其它类别（无权限 403 不能让弹窗哑火）', async () => {
  const loaders: UnreadLoaders = {
    notice: async () => 4,
    announcement: async () => {
      throw new Error('403 无权限');
    },
    task: async () => 1,
    approval: async () => {
      throw new Error('404 接口不存在');
    },
  };
  const counts = await collectUnread(loaders);
  assert.deepEqual(counts, { notice: 4, announcement: 0, task: 1, approval: 0 });
  assert.equal(totalUnread(counts), 5, '有 5 条未读 → 仍要弹');
});

test('collectUnread 缺 loader 的类别按 0 计（分阶段接接口时不会崩）', async () => {
  const counts = await collectUnread({ notice: async () => 2 });
  assert.deepEqual(counts, { notice: 2, announcement: 0, task: 0, approval: 0 });
  assert.deepEqual(emptyCounts(), { notice: 0, announcement: 0, task: 0, approval: 0 });
});

test('登录标记：未弹过 → 弹一次后标记 → 清标记后又能弹（退出登录再登录）', () => {
  resetShown();
  assert.equal(isLoginUnreadShown(), false, '首次登录：没弹过');
  markLoginUnreadShown();
  assert.equal(isLoginUnreadShown(), true, '弹过之后刷新页面不再弹');
  // 标记键名与卡面约定一致（E2E / 人工排查都按这个名字查 sessionStorage）
  assert.equal(LOGIN_UNREAD_SHOWN_KEY, 'login-unread-shown');
  clearLoginUnreadShown();
  assert.equal(isLoginUnreadShown(), false, '退出登录清标记 → 下次登录重新弹');
});

test('「有未读就弹 / 全空不弹」的判定口径：合计 > 0 才该弹', async () => {
  const none = await collectUnread({
    notice: async () => 0,
    announcement: async () => 0,
    task: async () => 0,
    approval: async () => 0,
  });
  assert.equal(totalUnread(none), 0, '四类全空 → 不弹');

  const some = await collectUnread({
    notice: async () => 0,
    announcement: async () => 0,
    task: async () => 0,
    approval: async () => 1,
  });
  assert.equal(totalUnread(some), 1, '只有一条审批待办也要弹');
});

test('计数容错：负数 / 空串 / null / NaN 一律按 0（后端下发 null 时不能显示「NaN 条」）', () => {
  const rows = summarizeUnread({
    notice: Number.NaN,
    announcement: -3,
    task: null as unknown as number,
    approval: '5' as unknown as number,
  });
  assert.deepEqual(
    rows.map((row) => row.count),
    [0, 0, 0, 5],
  );
});

test('类别清单是四条且键名与后端取数一一对应', () => {
  assert.deepEqual(
    UNREAD_SOURCES.map((source) => source.key),
    ['notice', 'announcement', 'task', 'approval'],
  );
  // 图标名都必须是 TDesign 里真实存在的（否则界面空白）
  assert.deepEqual(
    UNREAD_SOURCES.map((source) => source.icon),
    ['mail', 'notification', 'task', 'check-circle'],
  );
});

// ===== HOME-6：按权限发请求（无权限的类别既不请求也不出现在弹窗行里）=====

test('权限码逐字对应后端注解（改码就是 403，三处必须同步）', () => {
  assert.deepEqual(
    UNREAD_SOURCES.map((source) => [source.key, source.permission]),
    [
      ['notice', 'business:notice:mine'],
      ['announcement', 'business:announcement:read'],
      ['task', 'business:task:read'],
      ['approval', 'business:flow:instance:read'],
    ],
  );
});

test('permittedUnreadKeys：只返回有权限的类别，保持原顺序', () => {
  const all = permittedUnreadKeys(() => true);
  assert.deepEqual(all, ['notice', 'announcement', 'task', 'approval'], '超管（*:*:* 恒真）四类全查');

  // EMPLOYEE 这类账号：只有 task / approval
  const employee = permittedUnreadKeys((code) =>
    ['business:task:read', 'business:flow:instance:read'].includes(code),
  );
  assert.deepEqual(employee, ['task', 'approval']);

  // 四类都无权限 → 空数组（调用方据此整框不弹、一个请求都不发）
  assert.deepEqual(permittedUnreadKeys(() => false), []);
});

test('pickUnreadLoaders：无权限类别的 loader 被整条剔除，collectUnread 不会去调它', async () => {
  const called: string[] = [];
  const loaders: UnreadLoaders = {
    notice: async () => {
      called.push('notice');
      return 9;
    },
    announcement: async () => {
      called.push('announcement');
      return 9;
    },
    task: async () => {
      called.push('task');
      return 2;
    },
    approval: async () => {
      called.push('approval');
      return 3;
    },
  };
  const counts = await collectUnread(pickUnreadLoaders(loaders, ['task', 'approval']));
  assert.deepEqual(called, ['task', 'approval'], '只调有权限的两路');
  assert.deepEqual(counts, { notice: 0, announcement: 0, task: 2, approval: 3 });
  assert.equal(totalUnread(counts), 5);
});

test('summarizeUnread 传 keys：无权限的行根本不出现（不是置灰）', () => {
  const rows = summarizeUnread({ notice: 3, announcement: 2, task: 1, approval: 0 }, ['task', 'approval']);
  assert.deepEqual(
    rows.map((row) => [row.key, row.label, row.count]),
    [
      ['task', '工作任务', 1],
      ['approval', '审批待办', 0],
    ],
  );
});
