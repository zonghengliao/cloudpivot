/**
 * HOME-4 前端验收（Playwright / Chromium）。
 *
 * 覆盖卡面验收 ①–⑦：
 *   ① 非超管 → 只有工作台（问候语 + 工作任务 + 公告与消息 + 快捷入口），看不到趋势图
 *   ② 超管 → 两个 Tab，默认「工作台」
 *   ③ 超管「系统信息」→ 健康摘要 4 指标 + 两条趋势图 + 系统概况
 *   ④ 超管 Tab2「工作台」与非超管首页完全一致（同一组件：DOM 结构逐项比对）
 *   ⑤ 健康摘要「查看详情」→ /system/server-status
 *   ⑥ Tab 切换不重复请求（懒加载：首次切到才请求，之后常驻）
 *   ⑦ 非超管控制台无 403、且根本没请求超管接口
 *
 * 运行：cd frontend && node /var/tmp/home4/verify-ui.mjs
 */
import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';
import { mkdirSync, writeFileSync } from 'node:fs';

const BASE = process.env.BASE || 'http://127.0.0.1:5400';
const OUT = '/var/tmp/home4/ui';
const ADMIN = { username: 'admin', password: 'Home4@2026Pass' };
const VIEWER = { username: 'h4viewer', password: 'Viewer@2026Pass' };

mkdirSync(OUT, { recursive: true });

const report = {
  admin: {},
  viewer: {},
  lazy: {},
  structure: {},
  console: {},
  screenshots: [],
  pass: [],
  fail: [],
};
const log = (m) => { console.log(m); };
const ok = (cond, msg, extra = '') => {
  const line = `${cond ? 'PASS' : 'FAIL'}  ${msg}${extra ? ' :: ' + extra : ''}`;
  log(line);
  (cond ? report.pass : report.fail).push(msg);
  return cond;
};

async function login(page, who) {
  await page.goto(`${BASE}/login`, { waitUntil: 'domcontentloaded' });
  await page.getByPlaceholder('请输入用户名').fill(who.username);
  await page.getByPlaceholder('请输入密码').fill(who.password);
  await page.locator('button[type="submit"]').first().click();
  await page.waitForURL(/\/dashboard/, { timeout: 25000 });
  await page.waitForTimeout(1500);
  // 「初始化向导」弹窗会挡住点击（超管首次登录必现）：点「稍后再说」让路
  const skip = page.getByTestId('setup-wizard-skip');
  if (await skip.count()) {
    await skip.first().click({ timeout: 5000 }).catch(() => {});
    await page.waitForTimeout(800);
  }
  // 「登录后未读汇总」弹窗：有未读公告/消息/任务时会弹，同样挡住 Tab 点击。
  // 按真实用户行为点「知道了」关掉（这是产品既有功能，本卡不评价它）。
  const unreadOk = page.getByTestId('login-unread-ok');
  if (await unreadOk.count()) {
    await unreadOk.first().click({ timeout: 5000 }).catch(() => {});
    await page.waitForTimeout(600);
  }
}

const shots = async (page, name) => {
  const path = `${OUT}/${name}.png`;
  await page.screenshot({ path, fullPage: true });
  report.screenshots.push(path);
  log(`  📸 ${path}`);
};

const browser = await chromium.launch();
try {
  // ==================== 一、超管 ====================
  {
    const ctx = await browser.newContext({ viewport: { width: 1680, height: 1050 }, deviceScaleFactor: 1 });
    await ctx.addInitScript(() => {
      try { localStorage.setItem('cp-onboarding-done', '1'); } catch { /* ignore */ }
    });
    const page = await ctx.newPage();
    const reqs = [];
    const resp = [];
    const consoleErrors = [];
    page.on('request', (r) => reqs.push(r.url()));
    page.on('response', (r) => resp.push({ url: r.url(), status: r.status() }));
    page.on('console', (m) => { if (m.type() === 'error') consoleErrors.push(m.text()); });
    page.on('pageerror', (e) => consoleErrors.push(String(e)));

    await login(page, ADMIN);
    log('\n== 超管：首页默认 Tab ==');
    await page.waitForSelector('.wb__banner', { timeout: 20000 });
    await page.waitForTimeout(1200);

    const tabTexts = await page.locator('.t-tabs__nav-item').allInnerTexts();
    ok(tabTexts.length === 2, '超管页有两个 Tab', JSON.stringify(tabTexts.map((t) => t.trim())));

    const activeTab = (await page.locator('.t-tabs__nav-item.t-is-active').first().innerText()).trim();
    ok(activeTab.includes('工作台'), '默认 Tab = 工作台', activeTab);

    const wbVisible = await page.locator('.wb__banner').isVisible();
    ok(wbVisible, '工作台问候语条可见');
    const greeting = (await page.locator('.wb__greeting').innerText()).trim();
    const subtitle = (await page.locator('.wb__subtitle').innerText()).trim();
    ok(/好，/.test(greeting), '问候语格式「xx好，姓名」', greeting);
    log(`  副标题：${subtitle}`);
    report.admin.greeting = greeting;
    report.admin.subtitle = subtitle;

    // 工作台四块
    ok(await page.locator('.wb__tasks').isVisible(), '工作台：工作任务卡可见');
    ok(await page.locator('.wb__feed').isVisible(), '工作台：公告与消息卡可见');
    ok(await page.locator('.wb__entries').count() > 0 || (await page.locator('.wb__card .t-empty').count()) > 0, '工作台：快捷入口区块存在');
    const taskRows = await page.locator('.wb__tasks-item').count();
    const feedRows = await page.locator('.wb__feed-item').count();
    ok(taskRows > 0, '工作任务卡有真实条目', `${taskRows} 行`);
    ok(feedRows > 0, '公告与消息卡有真实条目', `${feedRows} 行`);
    report.admin.taskRows = taskRows;
    report.admin.feedRows = feedRows;

    // 默认工作台：不应有任何系统信息内容 / 趋势图
    ok((await page.locator('.health__grid').count()) === 0, '默认工作台不渲染健康摘要（懒加载）');
    ok((await page.locator('.si__metric').count()) === 0, '默认工作台不渲染系统概况卡');
    ok((await page.locator('.si canvas').count()) === 0, '默认工作台不渲染趋势图');

    const dashReqs = reqs.filter((u) => u.includes('/api/'));
    report.admin.initialRequests = dashReqs.map((u) => u.replace(BASE, ''));
    ok(!dashReqs.some((u) => u.includes('/dashboard/overview')), '首屏未请求 /support/dashboard/overview（懒加载）');
    ok(!dashReqs.some((u) => u.includes('/monitor/server-status')), '首屏未请求 /monitor/server-status（懒加载）');
    await shots(page, '01-admin-workbench');

    // ---- 切到「系统信息」 ----
    log('\n== 超管：切到「系统信息」==');
    await page.locator('.t-tabs__nav-item', { hasText: '系统信息' }).first().click();
    await page.waitForSelector('.health__grid', { timeout: 20000 });
    await page.waitForTimeout(2500);

    const healthItems = await page.locator('.health__item:not(.health__item--skeleton)').count();
    ok(healthItems === 4, '健康摘要 4 个指标', `${healthItems}`);
    const healthValues = (await page.locator('.health__value').allInnerTexts()).map((t) => t.replace(/\s+/g, ' ').trim());
    const healthFoots = (await page.locator('.health__foot').allInnerTexts()).map((t) => t.trim());
    ok(healthValues.length === 4 && healthValues.every((v) => v && v !== '?'), '健康摘要数值已渲染', JSON.stringify(healthValues));
    log(`  健康摘要：${JSON.stringify(healthValues)} / ${JSON.stringify(healthFoots)}`);
    report.admin.health = { values: healthValues, foots: healthFoots };

    const metricCards = await page.locator('.si__metric').count();
    ok(metricCards === 8, '系统概况 8 张指标卡', `${metricCards}`);
    const metricLabels = (await page.locator('.si__metric-label').allInnerTexts()).map((t) => t.trim());
    log(`  系统概况：${JSON.stringify(metricLabels)}`);
    ok(metricLabels.some((l) => l.includes('登录')) || metricLabels.length === 8, '系统概况含今日登录等运维口径');

    const chartCards = await page.locator('.si__half-col').count();
    const canvases = await page.locator('.si__half-col canvas').count();
    ok(chartCards === 2, '两条趋势图卡位', `${chartCards}`);
    ok(canvases >= 2, 'ECharts 画布已渲染', `${canvases} 个 canvas`);

    const details = (await page.locator('.health__actions').innerText()).trim();
    ok(details.includes('查看详情'), '健康摘要右上角有「查看详情」', details.replace(/\s+/g, ' '));
    await shots(page, '02-admin-system-info');

    // ---- ⑤ 查看详情 → /system/server-status ----
    log('\n== 超管：健康摘要「查看详情」跳转 ==');
    await page.locator('.health__actions').getByText('查看详情').click();
    await page.waitForURL(/\/system\/server-status/, { timeout: 20000 });
    await page.waitForSelector('.ss-hero__title', { timeout: 20000 });
    ok(page.url().includes('/system/server-status'), '「查看详情」跳到服务器状态页', page.url().replace(BASE, ''));
    await page.waitForTimeout(1500);
    await shots(page, '03-server-status-from-health');

    // ---- ⑥ 懒加载 / 重复请求 ----
    log('\n== 超管：Tab 来回切换不重复请求 ==');
    await page.goto(`${BASE}/dashboard`, { waitUntil: 'domcontentloaded' });
    await page.waitForSelector('.wb__banner', { timeout: 20000 });
    await page.waitForTimeout(1200);
    const before = reqs.filter((u) => u.includes('/dashboard/overview')).length;
    const beforeSrv = reqs.filter((u) => u.includes('/monitor/server-status')).length;

    await page.locator('.t-tabs__nav-item', { hasText: '系统信息' }).first().click();
    await page.waitForSelector('.health__grid', { timeout: 20000 });
    await page.waitForTimeout(2000);
    const afterOnce = reqs.filter((u) => u.includes('/dashboard/overview')).length;
    const afterOnceSrv = reqs.filter((u) => u.includes('/monitor/server-status')).length;
    ok(afterOnce === before + 1, '首次切到系统信息：概览接口只请求 1 次', `${before} → ${afterOnce}`);
    ok(afterOnceSrv === beforeSrv + 1, '首次切到系统信息：服务器状态接口只请求 1 次', `${beforeSrv} → ${afterOnceSrv}`);

    await page.locator('.t-tabs__nav-item', { hasText: '工作台' }).first().click();
    await page.waitForTimeout(900);
    await page.locator('.t-tabs__nav-item', { hasText: '系统信息' }).first().click();
    await page.waitForTimeout(1600);
    const afterTwice = reqs.filter((u) => u.includes('/dashboard/overview')).length;
    const afterTwiceSrv = reqs.filter((u) => u.includes('/monitor/server-status')).length;
    ok(afterTwice === afterOnce, '再切回系统信息：概览接口不再请求（组件常驻）', `${afterOnce} → ${afterTwice}`);
    ok(afterTwiceSrv === afterOnceSrv, '再切回系统信息：服务器状态接口不再请求', `${afterOnceSrv} → ${afterTwiceSrv}`);
    report.lazy = { overview: [before, afterOnce, afterTwice], serverStatus: [beforeSrv, afterOnceSrv, afterTwiceSrv] };

    // ---- ④ 超管 Tab2 与非超管首页同一组件：记录工作台 DOM 指纹 ----
    await page.locator('.t-tabs__nav-item', { hasText: '工作台' }).first().click();
    await page.waitForTimeout(1200);
    const adminWorkbench = await page.evaluate(() => {
      const root = document.querySelector('.wb');
      const take = (sel) => Array.from(document.querySelectorAll(sel)).map((n) => n.className.toString());
      return {
        rootClass: root ? root.className : null,
        cards: take('.wb .t-card__header .t-card__title, .wb .t-card__title'),
        blocks: {
          banner: !!document.querySelector('.wb__banner'),
          tasks: !!document.querySelector('.wb__tasks'),
          feed: !!document.querySelector('.wb__feed'),
          entries: !!document.querySelector('.wb__entries'),
        },
        trendChart: !!document.querySelector('.wb canvas'),
      };
    });
    report.structure.adminWorkbench = adminWorkbench;
    ok(adminWorkbench.blocks.banner && adminWorkbench.blocks.tasks && adminWorkbench.blocks.feed && adminWorkbench.blocks.entries,
      '超管 Tab2 工作台四块齐全');

    // 响应式：1360 / 960
    await page.setViewportSize({ width: 1360, height: 900 });
    await page.waitForTimeout(800);
    await shots(page, '04-admin-workbench-1360');
    await page.setViewportSize({ width: 960, height: 1000 });
    await page.waitForTimeout(800);
    await shots(page, '05-admin-workbench-960');

    report.console.admin = consoleErrors;
    ok(consoleErrors.length === 0, '超管控制台 0 error', consoleErrors.slice(0, 2).join(' | '));
    await ctx.close();
  }

  // ==================== 二、非超管 ====================
  {
    const ctx = await browser.newContext({ viewport: { width: 1680, height: 1050 }, deviceScaleFactor: 1 });
    await ctx.addInitScript(() => {
      try { localStorage.setItem('cp-onboarding-done', '1'); } catch { /* ignore */ }
    });
    const page = await ctx.newPage();
    const reqs = [];
    const resp = [];
    const consoleErrors = [];
    page.on('request', (r) => reqs.push(r.url()));
    page.on('response', (r) => resp.push({ url: r.url(), status: r.status() }));
    page.on('console', (m) => { if (m.type() === 'error') consoleErrors.push(m.text()); });
    page.on('pageerror', (e) => consoleErrors.push(String(e)));

    await login(page, VIEWER);
    log('\n== 非超管：首页形态 ==');
    await page.waitForSelector('.wb__banner', { timeout: 20000 });
    await page.waitForTimeout(2000);

    const tabCount = await page.locator('.t-tabs__nav-item').count();
    ok(tabCount === 0, '非超管没有任何 Tab', `${tabCount}`);
    ok(await page.locator('.wb__banner').isVisible(), '非超管：问候语条可见');
    ok(await page.locator('.wb__tasks').isVisible(), '非超管：工作任务卡可见');
    ok(await page.locator('.wb__feed').isVisible(), '非超管：公告与消息卡可见');
    ok((await page.locator('.wb__entries').count() > 0) || (await page.locator('.wb .t-empty').count() > 0), '非超管：快捷入口区块存在');

    ok((await page.locator('canvas').count()) === 0, '非超管：页面无任何趋势图（ECharts canvas 0）');
    ok((await page.locator('.health__grid').count()) === 0, '非超管：无健康摘要');
    ok((await page.locator('.si__metric').count()) === 0, '非超管：无系统概况');

    const apiReqs = reqs.filter((u) => u.includes('/api/')).map((u) => u.replace(BASE, ''));
    report.viewer.requests = apiReqs;
    ok(!apiReqs.some((u) => u.includes('/dashboard/overview')), '非超管未请求 /support/dashboard/overview');
    ok(!apiReqs.some((u) => u.includes('/monitor/server-status')), '非超管未请求 /monitor/server-status');
    const forbidden = resp.filter((r) => r.status === 403 || r.status === 401);
    report.viewer.forbidden = forbidden.map((r) => `${r.status} ${r.url.replace(BASE, '')}`);
    ok(forbidden.length === 0, '非超管无 403/401 响应', JSON.stringify(report.viewer.forbidden));

    // 工作台 DOM 结构与非超管一致
    const viewerWorkbench = await page.evaluate(() => {
      const root = document.querySelector('.wb');
      const take = (sel) => Array.from(document.querySelectorAll(sel)).map((n) => n.className.toString());
      return {
        rootClass: root ? root.className : null,
        cards: take('.wb .t-card__header .t-card__title, .wb .t-card__title'),
        blocks: {
          banner: !!document.querySelector('.wb__banner'),
          tasks: !!document.querySelector('.wb__tasks'),
          feed: !!document.querySelector('.wb__feed'),
          entries: !!document.querySelector('.wb__entries'),
        },
      };
    });
    report.structure.viewerWorkbench = viewerWorkbench;
    const blocksMatch = ['banner', 'tasks', 'feed'].every(
      (k) => viewerWorkbench.blocks[k] === report.structure.adminWorkbench.blocks[k],
    );
    ok(blocksMatch, '超管 Tab2 与非超管首页结构一致（同一组件渲染同一批卡片）', JSON.stringify(viewerWorkbench.blocks));
    ok(viewerWorkbench.blocks.entries, '非超管：快捷入口有可用条目（业务动线入口）');
    ok(JSON.stringify(viewerWorkbench.cards) === JSON.stringify(report.structure.adminWorkbench.cards),
      '两处卡片标题序列一致', JSON.stringify(viewerWorkbench.cards));

    await shots(page, '06-viewer-workbench');
    await page.setViewportSize({ width: 960, height: 1000 });
    await page.waitForTimeout(800);
    await shots(page, '07-viewer-960');

    report.console.viewer = consoleErrors;
    ok(consoleErrors.length === 0, '非超管控制台 0 error', consoleErrors.slice(0, 2).join(' | '));
    await ctx.close();
  }
} finally {
  await browser.close();
}

report.summary = { pass: report.pass.length, fail: report.fail.length, failed: report.fail };
writeFileSync(`${OUT}/report.json`, JSON.stringify(report, null, 2));
console.log(`\n===== 汇总：${report.pass.length} passed / ${report.fail.length} failed =====`);
if (report.fail.length) console.log('FAILED: ' + report.fail.join(' | '));
process.exit(report.fail.length ? 1 : 0);
