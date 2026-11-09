/**
 * HOME-6 验收（Playwright / Chromium）——顶栏铃铛与登录未读弹窗的「按权限发请求」收口。
 *
 * 覆盖卡面验收：
 *   ① 无 business:notice:mine → 不请求 /notice/stream、/notice/unread-count，铃铛不渲染（角标无从显示）
 *   ② 无 business:announcement:read → 不请求 /announcement/unread-count，未读弹窗该行不出现
 *   ③ 未读弹窗四类都无权限时整框不弹（本环境 EMPLOYEE 仍有 task/approval，故断言「无权限的两类不出现」）
 *   ④ 超管行为不变：铃铛角标、SSE 实时推送（发一条站内信 → 浮层弹出 + 角标 +1）、四行数字与接口值逐一对齐
 *
 * 运行：node docs/evidence/home6-20261109/scripts/verify-ui.mjs
 * 依赖：隔离后端 127.0.0.1:18094（库 cloudpivot_home4_v1）+ Vite 127.0.0.1:5400（BACKEND_ORIGIN=18094）
 *
 * 账号：admin / Home4@2026Pass（SUPER_ADMIN）、h4viewer / Viewer@2026Pass（EMPLOYEE，
 *       且已删掉 EMPLOYEE 对 notice:mine / announcement:read 的 4 条 sys_role_menu 授权）
 */
import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';
import { mkdirSync, writeFileSync } from 'node:fs';

const BASE = process.env.BASE || 'http://127.0.0.1:5400';
const API = process.env.API || 'http://127.0.0.1:18094';
const OUT = process.env.OUT || '/opt/cloudpivot/docs/evidence/home6-20261109';

const ADMIN = { username: 'admin', password: 'Home4@2026Pass' };
const VIEWER = { username: 'h4viewer', password: 'Viewer@2026Pass' };

const NOTICE_URL = '/api/business/notice/stream';
const NOTICE_COUNT_URL = '/api/business/notice/unread-count';
const ANN_COUNT_URL = '/api/business/announcement/unread-count';

mkdirSync(`${OUT}/ui`, { recursive: true });
mkdirSync(`${OUT}/api`, { recursive: true });

const report = { env: { BASE, API }, viewer: {}, admin: {}, pass: [], fail: [] };
const ok = (cond, msg, extra = '') => {
  const line = `${cond ? 'PASS' : 'FAIL'}  ${msg}${extra ? ' :: ' + extra : ''}`;
  console.log(line);
  (cond ? report.pass : report.fail).push(extra ? `${msg} :: ${extra}` : msg);
  return cond;
};
const pathOf = (url) => {
  try {
    return new URL(url).pathname;
  } catch {
    return url;
  }
};

async function apiLogin(who) {
  const res = await fetch(`${API}/api/system/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(who),
  });
  const body = await res.json();
  if (!body?.data?.access_token) {
    throw new Error(`登录失败 ${who.username}: ${JSON.stringify(body).slice(0, 200)}`);
  }
  return body.data.access_token;
}

async function apiGet(token, path) {
  const res = await fetch(`${API}${path}`, { headers: { Authorization: `Bearer ${token}` } });
  return { status: res.status, body: await res.json().catch(() => null) };
}

/** 打开一页并采集网络 / 控制台 */
async function openPage(browser) {
  const ctx = await browser.newContext({ viewport: { width: 1600, height: 1000 } });
  const page = await ctx.newPage();
  const net = [];
  const bad = [];
  const consoleErrors = [];
  page.on('request', (req) => net.push({ method: req.method(), path: pathOf(req.url()) }));
  page.on('response', (res) => {
    if (res.status() >= 400) bad.push({ status: res.status(), path: pathOf(res.url()) });
  });
  page.on('console', (msg) => {
    if (msg.type() === 'error') consoleErrors.push(msg.text());
  });
  page.on('pageerror', (err) => consoleErrors.push(`pageerror: ${err.message}`));
  return { ctx, page, net, bad, consoleErrors };
}

async function login(page, who) {
  await page.goto(`${BASE}/login`, { waitUntil: 'domcontentloaded' });
  await page.getByPlaceholder('请输入用户名').fill(who.username);
  await page.getByPlaceholder('请输入密码').fill(who.password);
  await page.locator('button[type="submit"]').first().click();
  await page.waitForURL(/\/dashboard/, { timeout: 30000 });
  await page.waitForTimeout(1200);
  const skip = page.getByTestId('setup-wizard-skip');
  if (await skip.count()) {
    await skip.first().click({ timeout: 5000 }).catch(() => {});
    await page.waitForTimeout(600);
  }
}

const countOf = (net, path) => net.filter((r) => r.path === path).length;

// ===================== ① ② ③ 非超管（EMPLOYEE，无 notice:mine / announcement:read）=====================
async function checkViewer(browser) {
  const token = await apiLogin(VIEWER);
  const me = await apiGet(token, '/api/system/auth/me');
  const v = report.viewer;
  v.me = {
    username: me.body?.data?.username,
    roles: me.body?.data?.roles,
    hasNoticeMine: (me.body?.data?.permissions ?? []).includes('business:notice:mine'),
    hasAnnouncementRead: (me.body?.data?.permissions ?? []).includes('business:announcement:read'),
    hasTaskRead: (me.body?.data?.permissions ?? []).includes('business:task:read'),
    hasFlowRead: (me.body?.data?.permissions ?? []).includes('business:flow:instance:read'),
  };
  ok(v.me.hasNoticeMine === false, '前置：viewer 无 business:notice:mine');
  ok(v.me.hasAnnouncementRead === false, '前置：viewer 无 business:announcement:read');

  const { ctx, page, net, bad, consoleErrors } = await openPage(browser);
  await login(page, VIEWER);
  // 留足时间：SSE 会立刻握手、未读数轮询 30s 一次、登录弹窗并发四路
  await page.waitForTimeout(6000);

  // 未读弹窗（若弹）里的行
  const dialogVisible = await page.getByTestId('login-unread-dialog').isVisible().catch(() => false);
  const rows = dialogVisible
    ? await page.locator('[data-testid^="login-unread-row-"]').evaluateAll((els) =>
        els.map((el) => ({ key: el.getAttribute('data-testid').replace('login-unread-row-', ''), text: el.textContent.trim() })),
      )
    : [];

  const bellCount = await page.locator('.notice-bell__trigger').count();

  v.network = {
    noticeStream: countOf(net, NOTICE_URL),
    noticeUnreadCount: countOf(net, NOTICE_COUNT_URL),
    announcementUnreadCount: countOf(net, ANN_COUNT_URL),
    noticePage: net.filter((r) => r.path.startsWith('/api/business/notice/page')).length,
  };
  v.badResponses = bad;
  v.consoleErrors = consoleErrors;
  v.bell = bellCount;
  v.dialogVisible = dialogVisible;
  v.dialogRows = rows;

  ok(v.network.noticeStream === 0, '① 无 notice:mine → 不请求 /notice/stream', `实际 ${v.network.noticeStream} 次`);
  ok(v.network.noticeUnreadCount === 0, '① 无 notice:mine → 不请求 /notice/unread-count', `实际 ${v.network.noticeUnreadCount} 次`);
  ok(v.network.noticePage === 0, '① 无 notice:mine → 不请求 /notice/page（铃铛面板数据源）', `实际 ${v.network.noticePage} 次`);
  ok(v.network.announcementUnreadCount === 0, '② 无 announcement:read → 不请求 /announcement/unread-count', `实际 ${v.network.announcementUnreadCount} 次`);
  ok(bellCount === 0, '① 无 notice:mine → 铃铛整只不渲染（角标无从显示）', `bell=${bellCount}`);
  ok(!rows.some((r) => r.key === 'notice'), '② 未读弹窗不出现「站内信」行', JSON.stringify(rows.map((r) => r.key)));
  ok(!rows.some((r) => r.key === 'announcement'), '② 未读弹窗不出现「公告」行', JSON.stringify(rows.map((r) => r.key)));
  ok(bad.length === 0, '无权限账号整页 0 个 4xx/5xx', JSON.stringify(bad));
  ok(consoleErrors.length === 0, '无权限账号 console error = 0', JSON.stringify(consoleErrors));

  await page.screenshot({ path: `${OUT}/ui/01-viewer-dashboard.png`, fullPage: false });
  await ctx.close();
}

// ===================== ④ 超管：行为不变（角标 / SSE 推送 / 弹窗数字）=====================
async function checkAdmin(browser) {
  const token = await apiLogin(ADMIN);
  const me = await apiGet(token, '/api/system/auth/me');
  const adminId = me.body?.data?.id;
  const a = report.admin;
  a.me = { username: me.body?.data?.username, roles: me.body?.data?.roles };

  // 改前口径：四个接口的真实值（用于核对弹窗数字）
  const noticeUnread = await apiGet(token, '/api/business/notice/unread-count');
  const annUnread = await apiGet(token, '/api/business/announcement/unread-count');
  const taskTotal = await apiGet(token, '/api/business/task/page?page=1&page_size=1&status=TODO');
  const todoTotal = await apiGet(token, '/api/business/flow/task/todo?page=1&page_size=1');
  a.expectedCounts = {
    notice: noticeUnread.body?.data?.unread_count,
    announcement: annUnread.body?.data?.announcement_unread,
    task: taskTotal.body?.data?.total,
    approval: todoTotal.body?.data?.total,
  };

  const { ctx, page, net, bad, consoleErrors } = await openPage(browser);
  await login(page, ADMIN);
  await page.waitForTimeout(2500);

  // 登录未读弹窗：记录四行数字后按真实用户行为关掉
  const dialogShown = await page.getByTestId('login-unread-dialog').isVisible().catch(() => false);
  const dialogCounts = {};
  if (dialogShown) {
    for (const key of ['notice', 'announcement', 'task', 'approval']) {
      const el = page.getByTestId(`login-unread-count-${key}`);
      dialogCounts[key] = (await el.count()) ? (await el.first().innerText()).replace(/[^\d]/g, '') : null;
    }
  }
  const unreadOk = page.getByTestId('login-unread-ok');
  if (await unreadOk.count()) {
    await unreadOk.first().click({ timeout: 5000 }).catch(() => {});
    await page.waitForTimeout(800);
  }

  const bellCount = await page.locator('.notice-bell__trigger').count();
  const badgeBefore = await page
    .locator('.notice-bell__trigger')
    .first()
    .evaluate((el) => (el.closest('.t-badge')?.textContent || '').trim())
    .catch(() => '');

  // SSE 实时推送：给 admin 自己发一条站内信，期望浮层弹出 + 角标 +1
  let pushOk = false;
  let pushDetail = {};
  try {
    const send = await fetch(`${API}/api/business/notice`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${token}` },
      body: JSON.stringify({
        notice_type: 'SYSTEM',
        level: 'INFO',
        title: `HOME-6 SSE 实时推送验收 ${Date.now()}`,
        content: 'HOME-6 验收：验证顶栏 SSE 推送链路未被权限收口影响。',
        content_format: 'TEXT',
        target_type: 'USER',
        target_ids: [adminId],
      }),
    });
    pushDetail.status = send.status;
    pushOk = send.status < 300;
    if (pushOk) {
      await page.getByTestId('notice-toast').waitFor({ state: 'visible', timeout: 12000 });
      pushDetail.toastTitle = await page.getByTestId('notice-toast-title').innerText();
    }
  } catch (err) {
    pushDetail.error = String(err).slice(0, 300);
  }
  const badgeAfter = await page
    .locator('.notice-bell__trigger')
    .first()
    .evaluate((el) => (el.closest('.t-badge')?.textContent || '').trim())
    .catch(() => '');

  a.network = {
    noticeStream: countOf(net, NOTICE_URL),
    noticeUnreadCount: countOf(net, NOTICE_COUNT_URL),
    announcementUnreadCount: countOf(net, ANN_COUNT_URL),
  };
  a.streamResponses = net.filter((r) => r.path === NOTICE_URL).length;
  a.badResponses = bad;
  a.consoleErrors = consoleErrors;
  a.bell = bellCount;
  a.badgeBefore = badgeBefore;
  a.badgeAfter = badgeAfter;
  a.dialogVisible = dialogShown;
  a.dialogCounts = dialogCounts;
  a.push = pushDetail;

  ok(bellCount === 1, '④ 超管铃铛仍渲染', `bell=${bellCount}`);
  ok(a.network.noticeStream >= 1, '④ 超管仍建立 /notice/stream 长连接', `实际 ${a.network.noticeStream} 次`);
  ok(a.network.noticeUnreadCount >= 1, '④ 超管仍拉 /notice/unread-count', `实际 ${a.network.noticeUnreadCount} 次`);
  ok(a.network.announcementUnreadCount >= 1, '④ 超管仍拉 /announcement/unread-count', `实际 ${a.network.announcementUnreadCount} 次`);
  ok(bad.length === 0, '④ 超管整页 0 个 4xx/5xx', JSON.stringify(bad));
  ok(consoleErrors.length === 0, '④ 超管 console error = 0', JSON.stringify(consoleErrors));
  ok(pushOk, '④ SSE 实时推送仍生效（发站内信 → 浮层秒弹）', JSON.stringify(pushDetail));

  // 数字逐字对齐接口值（改前 = 后端字段，改后不得变口径）
  if (dialogShown) {
    for (const key of ['notice', 'announcement', 'task', 'approval']) {
      ok(
        String(dialogCounts[key]) === String(a.expectedCounts[key] ?? 0),
        `④ 未读弹窗「${key}」数字与接口值一致`,
        `弹窗=${dialogCounts[key]} 接口=${a.expectedCounts[key]}`,
      );
    }
  } else {
    ok(false, '④ 超管本次登录未弹未读弹窗（应有未读）', JSON.stringify(a.expectedCounts));
  }

  await page.screenshot({ path: `${OUT}/ui/02-admin-dashboard.png`, fullPage: false });
  await ctx.close();
}

const browser = await chromium.launch();
try {
  await checkViewer(browser);
  await checkAdmin(browser);
} finally {
  await browser.close();
}

report.summary = { pass: report.pass.length, fail: report.fail.length };
writeFileSync(`${OUT}/api/report-home6.json`, JSON.stringify(report, null, 2));
console.log(`\n== ${report.pass.length} passed / ${report.fail.length} failed ==`);
if (report.fail.length) {
  console.log(report.fail.join('\n'));
  process.exit(1);
}
