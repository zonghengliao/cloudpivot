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

// ==================== 页面 / 拦截器契约（源码级断言） ====================
//
// 为什么要读源码断言：验收 ⑮⑯（非超管看不到「禁用」、超管能看到）、⑪（轮询不重置计时）
// 这三条是**渲染 / 接线**层面的口径，纯函数测不到；本仓既有做法就是用源码断言钉死
// （见 `tests/unit/attachment-gate-pages.test.ts`）。它们防的是「后人顺手把条件删了」。

import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const SRC = resolve(HERE, '../../src');

/** 读取前端源码（相对 `frontend/src`） */
function readSource(relative: string): string {
  return readFileSync(join(SRC, relative), 'utf8');
}

test('配置页：空闲超时参数只在超管侧出现「禁用」选项（⑮⑯）', () => {
  const page = readSource('views/support/config/index.vue');
  // 超管：下拉框（含 0 = 禁用），条件是 isIdleTimeoutKey + isSuperAdmin 同时成立
  assert.match(
    page,
    /v-if="isIdleTimeoutKey\(item\) && userStore\.isSuperAdmin"[\s\S]{0,200}:options="IDLE_TIMEOUT_OPTIONS"/,
    '超管分支必须是「本键 + isSuperAdmin」双条件，否则普通管理员也会看到下拉框',
  );
  // 非超管：数字输入框，且区间钉死 1-60（不含禁用）
  assert.match(
    page,
    /v-else-if="isIdleTimeoutKey\(item\)"[\s\S]{0,200}:min="1"[\s\S]{0,80}:max="60"/,
    '非超管分支必须是 1-60 数字框',
  );
  // 下拉项里必须有「禁用自动退出」= 0，且只有这一个 0 值选项
  assert.match(page, /\{\s*label:\s*'禁用自动退出',\s*value:\s*0\s*\}/, '缺少「禁用自动退出」选项');
  assert.equal((page.match(/value:\s*0\s*\}/g) ?? []).length, 1, '下拉里只允许出现一个 0 值选项');
  // 前端也要先拦一道（后端仍是兜底）
  assert.match(page, /只有超级管理员可以禁用自动退出登录/, '前端缺少非超管设 0 的拦截文案');
});

test('登录页：回跳走 safeRedirect，并消费 sessionStorage 里的原页面（⑰⑱）', () => {
  const page = readSource('views/login/index.vue');
  assert.match(page, /safeRedirect\(savedRedirect \|\| queryRedirect\)/, '回跳必须统一过 safeRedirect');
  assert.match(page, /takeIdleRedirect\(\)/, '必须读取空闲退出时记下的原页面');
  assert.match(page, /因长时间未操作已自动退出/, '登录页必须提示自动退出的原因');
  // 不允许再出现「直接把 query 拿去 replace」的旧写法
  assert.doesNotMatch(page, /router\.replace\(redirect\)/, '不允许绕过 safeRedirect 直接回跳');
});

test('axios 拦截器：只有真实操作才打活跃标记，轮询 GET 不带（⑪）', () => {
  const request = readSource('api/request.ts');
  assert.match(
    request,
    /config\.headers\[USER_ACTIVE_HEADER\] = isRecentlyActive\(\) \? '1' : '0'/,
    '请求头必须由「最近是否有操作」决定，不能无条件打 1',
  );
  // 反向证据：拦截器里不能因为「有请求」就把时间戳刷新（那样轮询会一直续期）
  assert.doesNotMatch(request, /markActivity\(/, '拦截器不得因请求刷新活跃时间戳');
  assert.match(request, /AUTH_IDLE_TIMEOUT_CODE = 20009/, '必须识别服务端 20009');
  assert.match(request, /redirectToLogin\('idle'\)/, '20009 必须带 reason=idle 回登录页');
});

test('空闲守卫挂在根组件（改密页也要计时）', () => {
  const app = readSource('App.vue');
  assert.match(app, /<IdleLogoutGuard \/>/, '根组件必须挂空闲守卫');
});

test('多标签：退出广播与跨标签时间戳同步都在（⑨）', () => {
  const composable = readSource('composables/useIdleLogout.ts');
  assert.match(composable, /window\.addEventListener\('storage', onStorage\)/, '必须监听 storage 事件');
  assert.match(composable, /IDLE_LOGOUT_BROADCAST_KEY/, '必须处理退出广播');
  // 不能用 requestIdleCallback（卡面明确要求：后台标签页不触发）——
  // 只查「调用」形态（`requestIdleCallback(`），注释里说明「不要用」是允许的
  assert.doesNotMatch(composable, /requestIdleCallback\s*\(/, '不要用 requestIdleCallback');
  // 判定靠「距离上次操作多久」，而不是「距离上次请求多久」
  assert.match(composable, /idleMillis\(\)/, '判定必须基于用户操作时间戳');
});
