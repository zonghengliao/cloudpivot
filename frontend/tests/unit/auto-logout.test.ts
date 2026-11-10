/**
 * AUTO-LOGOUT（空闲自动退出登录）的**纯逻辑**单测。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * # 为什么测这两个 `.ts` 而不测 `.vue`
 *
 * 卡面（2026-11-10）里最容易出错、也最该被钉死的两条判定都是纯函数：
 *
 * 1. **什么算「有操作」**（`utils/idle-activity.ts`）—— 判定错一次，要么「打开首页
 *    挂机永远不超时」（把轮询算成操作），要么「正常点着也定时被踢」（窗口太长/太短）。
 * 2. **回跳地址是否安全**（`utils/safe-redirect.ts`）—— 判定错一次就是开放重定向
 *    （验收 ⑱：`//evil.com` / `http://evil.com` 必须回首页）。
 *
 * `composables/useIdleLogout.ts` 与 `IdleLogoutGuard.vue` 只负责挂定时器与画弹窗，
 * 它们调用的判定全在下面这些函数里。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import {
  ACTIVE_REQUEST_WINDOW_MS,
  IDLE_ACTIVITY_KEY,
  IDLE_REDIRECT_KEY,
  USER_ACTIVE_HEADER,
  idleMillis,
  isRecentlyActive,
  lastActivity,
  markActivity,
  resetActivity,
  saveIdleRedirect,
  takeIdleRedirect,
} from '@/utils/idle-activity';
import { DEFAULT_REDIRECT, safeRedirect } from '@/utils/safe-redirect';

/** 固定「当前时刻」，避免用例依赖真实时钟 */
const NOW = 1_760_000_000_000;

// ==================== 回跳地址（防开放重定向） ====================

test('safe-redirect: 站内相对路径原样放行', () => {
  for (const path of ['/dashboard', '/business/sales/order', '/business/sales/order?id=1#top', '/']) {
    assert.equal(safeRedirect(path), path, `path=${path}`);
  }
});

test('safe-redirect: 外站 / 协议 / 反斜杠 / 控制字符一律回首页', () => {
  const hostile = [
    '//evil.com',
    '//evil.com/x',
    'http://evil.com',
    'https://evil.com/x',
    'javascript:alert(1)',
    'evil.com',
    '/\\\\evil.com',
    '/\\evil.com',
    '/a://evil.com',
    '/dashboard\nX-Injected: 1',
    '\u0000/dashboard',
    '  //evil.com  ',
    '',
    null,
    undefined,
    42,
    { path: '/dashboard' },
  ];
  for (const raw of hostile) {
    assert.equal(safeRedirect(raw), DEFAULT_REDIRECT, `raw=${JSON.stringify(raw)}`);
  }
});

test('safe-redirect: 允许自定义兜底目标', () => {
  assert.equal(safeRedirect('//evil.com', '/result/403'), '/result/403');
});

// ==================== 「什么算有操作」 ====================

test('idle-activity: 标记后窗口内算「刚刚有操作」', () => {
  markActivity(NOW);
  assert.equal(idleMillis(NOW), 0);
  assert.equal(isRecentlyActive(NOW), true);
  assert.equal(isRecentlyActive(NOW + ACTIVE_REQUEST_WINDOW_MS), true, '窗口边界仍算活跃');
});

test('idle-activity: 超过窗口不再算活跃（否则轮询会一直续期）', () => {
  markActivity(NOW);
  assert.equal(isRecentlyActive(NOW + ACTIVE_REQUEST_WINDOW_MS + 1), false);
  // 关键口径（卡面验收 ⑩⑪）：没有任何标记 → 10 分钟后的请求不带活跃标记，
  // 首页的自动轮询因此不会把空闲计时重置
  assert.equal(isRecentlyActive(NOW + 10 * 60_000), false);
});

test('idle-activity: 有记录时以记录为准，兜底值只在真的没有记录时才用', () => {
  // 模块内的时间戳是跨用例共享的（同一个进程），所以这里先把记录钉在 NOW
  markActivity(NOW);
  // 记录优先：兜底值更旧或更新都不该顶掉真实记录，
  // 否则「上次操作时间」会被一个未来的兜底值污染 → 永远不超时
  assert.equal(lastActivity(NOW - 60_000), NOW);
  assert.equal(lastActivity(NOW + 60_000), NOW);
  assert.equal(idleMillis(NOW + 60_000), 60_000);
});

test('idle-activity: 重新标记会推进时间戳', () => {
  markActivity(NOW);
  const later = NOW + 5 * 60_000;
  markActivity(later);
  assert.equal(idleMillis(later), 0);
  assert.equal(idleMillis(later + 60_000), 60_000);
});

test('idle-activity: resetActivity 等价于「此刻刚刚有操作」', () => {
  markActivity(NOW);
  const later = NOW + 30 * 60_000;
  resetActivity(later);
  assert.equal(idleMillis(later), 0);
});

test('idle-activity: 存储键与请求头名是与后端约定的契约', () => {
  // 后端 `idle_session::HEADER_USER_ACTIVE` / 前端各自持有一份，改名字要两边一起改
  assert.equal(USER_ACTIVE_HEADER, 'X-User-Active');
  assert.equal(IDLE_ACTIVITY_KEY, 'idle:last-active-at');
  assert.equal(IDLE_REDIRECT_KEY, 'idle_logout_redirect');
});

test('idle-activity: Node 环境（无 localStorage）不抛错', () => {
  // 单测跑在 Node 下，没有 window/localStorage：所有函数必须安全降级为纯内存
  assert.equal(typeof globalThis.window, 'undefined');
  assert.doesNotThrow(() => markActivity(NOW));
  assert.doesNotThrow(() => saveIdleRedirect('/business/sales/order'));
  // 无存储时取不出内容，返回空串（登录页会因此回落到 ?redirect 或首页）
  assert.equal(takeIdleRedirect(), '');
});
