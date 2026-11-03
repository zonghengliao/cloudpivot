/**
 * 新通知浮层（右上角自动弹）核心规则单测。
 *
 * 被测对象是 `src/utils/notice-popup.ts` / `src/utils/notice-stream.ts`：
 * `stores/notice.ts` 的 `latestItems` 就是 `NoticeAccumulator` 的输出，`NoticeToast.vue` 的倒计时
 * 就是 `AutoCloseTimer`，所以这里覆盖的就是卡面要求的「store 累积与上限 / 自动收起 / 悬停暂停」。
 *
 * 为什么不在测试里直接 import store：`@/api/request` 顶层用了 `import.meta.env`，
 * 在 `tsx --test`（纯 Node，无 vite）下会抛 `TypeError: Cannot read properties of undefined`。
 *
 * 运行：`pnpm test:unit`（等价 `tsx --test` 扫 `tests/unit` 下的用例文件）
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  AutoCloseTimer,
  MAX_POPUP_ITEMS,
  NoticeAccumulator,
  appendPopupItem,
  filterArrivedSince,
  formatRelativeTime,
  levelKey,
  normalizePopupItem,
  noticeTypeMeta,
  type NoticePopupItem,
} from '../../src/utils/notice-popup';
import { createSseState, parseSseChunk } from '../../src/utils/notice-stream';

/** 造一条 SSE 帧形态的原始数据（字段照 2026-11-03 真回执抄） */
function rawNotice(id: string, extra: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    notice_id: `notice-${id}`,
    notice_no: `NTT20261103${id}`,
    notice_type: 'TODO',
    level: 'INFO',
    title: `通知 ${id}`,
    link_url: null,
    biz_type: null,
    biz_id: null,
    receiver_id: id,
    created_at: '2026-11-03T14:49:36.573767871+00:00',
    ...extra,
  };
}

/** 睡眠（计时器用例用真实 setTimeout，时长取 20~40ms 保证稳定） */
function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => {
    setTimeout(resolve, ms);
  });
}

// ===== A. 累积 / 上限 / 去重（store 的 latestItems 语义） =====

test('latestItems 累积：后到的排在最前，条数逐条递增', () => {
  const acc = new NoticeAccumulator();
  assert.equal(acc.push(rawNotice('a')).accepted, true);
  assert.equal(acc.push(rawNotice('b')).accepted, true);
  assert.equal(acc.push(rawNotice('c')).accepted, true);
  assert.equal(acc.count, 3);
  assert.deepEqual(
    acc.current.map((item) => item.receiver_id),
    ['c', 'b', 'a'],
  );
});

test('latestItems 上限：第 6 条进来挤掉最旧的一条，恒不超过 5 条', () => {
  const acc = new NoticeAccumulator();
  for (const id of ['a', 'b', 'c', 'd', 'e', 'f']) {
    acc.push(rawNotice(id));
  }
  assert.equal(acc.count, MAX_POPUP_ITEMS);
  assert.deepEqual(
    acc.current.map((item) => item.receiver_id),
    ['f', 'e', 'd', 'c', 'b'],
  );
});

test('同一条消息（同 receiver_id）从不同通道重复送到时只入队一次', () => {
  const acc = new NoticeAccumulator();
  assert.equal(acc.push(rawNotice('x')).accepted, true);
  // 第二条：SSE 又推一次 / 「拉最新 5 条」又带回来
  assert.equal(acc.push(rawNotice('x')).accepted, false);
  assert.equal(acc.count, 1);
  // 关闭浮层后，同一条**仍然**不会再弹（seen 跨批次保留）
  acc.clear();
  assert.equal(acc.push(rawNotice('x')).accepted, false);
  assert.equal(acc.count, 0);
  // 但新的一条照常入队
  assert.equal(acc.push(rawNotice('y')).accepted, true);
  assert.equal(acc.count, 1);
});

test('脏数据（缺 receiver_id / 缺 title / 非对象）不入队，也不抛错', () => {
  const acc = new NoticeAccumulator();
  assert.equal(acc.push(null).accepted, false);
  assert.equal(acc.push('not-json').accepted, false);
  assert.equal(acc.push({ title: '没有 receiver_id' }).accepted, false);
  assert.equal(acc.push({ receiver_id: 'r1' }).accepted, false);
  assert.equal(acc.count, 0);
  assert.equal(normalizePopupItem({ receiver_id: 'r1', title: 'ok' })?.level, 'INFO');
});

// ===== B. 自动收起计时 / 悬停暂停 =====

test('计时到期自动收起（8 秒的短时长等价用例）', async () => {
  let closed = 0;
  const timer = new AutoCloseTimer(() => {
    closed += 1;
  }, 40);
  timer.start();
  assert.equal(timer.active, true);
  await sleep(80);
  assert.equal(closed, 1, '到期应触发一次收起');
  assert.equal(timer.active, false);
});

test('鼠标悬停暂停计时：暂停期间不收起，离开后按剩余时间继续', async () => {
  let closed = 0;
  const timer = new AutoCloseTimer(() => {
    closed += 1;
  }, 60);
  timer.start();
  await sleep(20);
  timer.pause();
  assert.equal(timer.isPaused, true);
  await sleep(80);
  assert.equal(closed, 0, '悬停期间不得自动收起');
  timer.resume();
  assert.equal(timer.isPaused, false);
  await sleep(80);
  assert.equal(closed, 1, '离开后剩余时间走完才收起');
});

test('新消息到达时重新计满：两条消息间隔小于 8 秒也只收起一次', async () => {
  let closed = 0;
  const timer = new AutoCloseTimer(() => {
    closed += 1;
  }, 50);
  timer.start();
  await sleep(35);
  timer.start(); // 第二条消息到达 → 重新计时
  await sleep(35);
  assert.equal(closed, 0, '第二条消息应把计时续上');
  await sleep(60);
  assert.equal(closed, 1, '只收起一次，不会叠两个浮层');
});

test('cancel 之后不再触发收起（用户点 [×] 的情形）', async () => {
  let closed = 0;
  const timer = new AutoCloseTimer(() => {
    closed += 1;
  }, 30);
  timer.start();
  timer.cancel();
  await sleep(60);
  assert.equal(closed, 0);
  assert.equal(timer.active, false);
});

// ===== C. 展示口径 =====

test('按 notice_type 区分图标、按 level 区分语义色、相对时间文案正确', () => {
  assert.equal(noticeTypeMeta('WORKFLOW').icon, 'check-circle');
  assert.equal(noticeTypeMeta('CIRCULAR').label, '公告');
  assert.equal(noticeTypeMeta('SOMETHING_NEW').icon, 'notification', '未知类型回落，不空白');
  assert.equal(levelKey('ERROR'), 'error');
  assert.equal(levelKey('URGENT'), 'warn');
  assert.equal(levelKey(null), 'info');

  const now = Date.parse('2026-11-03T15:00:00+00:00');
  assert.equal(formatRelativeTime('2026-11-03T14:59:30+00:00', now), '刚刚');
  assert.equal(formatRelativeTime('2026-11-03T14:55:00+00:00', now), '5 分钟前');
  assert.equal(formatRelativeTime('2026-11-03T12:00:00+00:00', now), '3 小时前');
  assert.equal(formatRelativeTime('2026-10-31T12:00:00+00:00', now), '3 天前');
  assert.equal(formatRelativeTime(null, now), '-');
});

test('appendPopupItem 不修改入参（store 里是 ref 数组，必须换新引用才触发渲染）', () => {
  const list: NoticePopupItem[] = [
    {
      receiver_id: 'a',
      notice_type: 'TODO',
      level: 'INFO',
      title: 'A',
      created_at: '2026-11-03T00:00:00+00:00',
    },
  ];
  const next = appendPopupItem(list, normalizePopupItem(rawNotice('b')) as NoticePopupItem);
  assert.equal(list.length, 1, '原数组不得被就地修改');
  assert.equal(next.length, 2);
  assert.equal(next[0].receiver_id, 'b');
});

// ===== D. SSE 帧解析（带头流通道） =====

test('SSE 帧解析：跨 chunk 的半截帧要拼接完整后才产出', () => {
  const state = createSseState();
  const frame = `event: notice\ndata: ${JSON.stringify(rawNotice('p1'))}\n\n`;
  const cut = 30;
  assert.deepEqual(parseSseChunk(state, frame.slice(0, cut)), [], '半截帧不得产出');
  const frames = parseSseChunk(state, frame.slice(cut));
  assert.equal(frames.length, 1);
  assert.equal(frames[0].event, 'notice');
  assert.equal(JSON.parse(frames[0].data).receiver_id, 'p1');
});

test('SSE 心跳注释帧（`: keep-alive`）被忽略，不影响后续数据帧', () => {
  const state = createSseState();
  const frames = parseSseChunk(state, ': keep-alive\n\n');
  assert.deepEqual(frames, []);
  const next = parseSseChunk(state, `event: notice\ndata: {"receiver_id":"k1"}\n\n`);
  assert.equal(next.length, 1);
  assert.equal(JSON.parse(next[0].data).receiver_id, 'k1');
});

// ===== E. 「只弹这次真的新到的」过滤（真机验收发现过的 bug） =====

test('filterArrivedSince 只留新到的：旧未读不因「拉最新 5 条」被误报成新消息', () => {
  const now = Date.parse('2026-11-03T15:10:00+00:00');
  const rows = [
    { receiver_id: 'new', created_at: '2026-11-03T15:09:58+00:00' }, // 2 秒前（新到）
    { receiver_id: 'old1', created_at: '2026-11-03T14:50:00+00:00' }, // 20 分钟前（旧未读）
    { receiver_id: 'old2', created_at: '2026-11-03T09:00:00+00:00' },
  ];
  const kept = filterArrivedSince(rows, now - 5_000);
  assert.deepEqual(
    kept.map((row) => row.receiver_id),
    ['new'],
  );
});

test('filterArrivedSince 对脏 created_at 保留（宁可多显示一条，也不吞掉新消息）', () => {
  const kept = filterArrivedSince([{ receiver_id: 'dirty', created_at: null }], Date.now() - 5_000);
  assert.equal(kept.length, 1);
});
