/**
 * HOME-3 前端验收（Playwright / Chromium）。
 *
 * 覆盖：
 *   ① 超管侧栏「系统管理 → 服务器状态」可点进（URL + 标题）
 *   ② 四张指标卡有真实数值 + 环形进度按实际值收敛 + CSS transition 800ms
 *   ③ 两张趋势图在拿到 ≥2 个采样点后画出折线（ECharts canvas 存在）
 *   ④ 服务健康清单 6 行、Hermes 显示「未安装」
 *   ⑤ 环境信息 4 块（版本 / commit / 迁移 / 库名 …）
 *   ⑥ 刷新按钮可用、下载诊断包能落盘
 *   ⑦ 卡片 hover 有位移（translateY）
 *   ⑧ 非超管：侧栏没有「服务器状态」，直接访问 /system/server-status → /result/403
 *
 * 运行：node verify-ui.mjs（工作目录 = 本目录；需能 import @playwright/test）
 */
import { chromium } from '@playwright/test';
import { mkdirSync, writeFileSync } from 'node:fs';

const BASE = process.env.BASE || 'http://127.0.0.1:5399';
const OUT = '/var/tmp/home3-evidence/ui';
const ADMIN = { username: 'admin', password: process.env.ADMIN_PASSWORD || 'Home3@2026Pass' };
const VIEWER = { username: 'h3viewer', password: 'Viewer@2026Pass' };

mkdirSync(OUT, { recursive: true });

const report = { steps: [], metrics: {}, health: [], env: {}, charts: {}, download: {}, viewer: {}, hover: {}, ring: {} };
const log = (msg) => {
  report.steps.push(msg);
  console.log(msg);
};

async function login(page, who) {
  await page.goto(`${BASE}/login`, { waitUntil: 'domcontentloaded' });
  await page.getByPlaceholder('请输入用户名').fill(who.username);
  await page.getByPlaceholder('请输入密码').fill(who.password);
  await page.locator('button[type="submit"]').first().click();
  await page.waitForURL(/\/dashboard/, { timeout: 20000 });
  await page.waitForTimeout(1200);
  // 「初始化向导」弹窗会挡住侧栏点击（超管首次登录必然出现）：点「稍后再说」关掉。
  // 该弹窗是产品既有功能，与本卡无关，验收里不评价它，只负责让路。
  const skip = page.getByTestId('setup-wizard-skip');
  if (await skip.count()) {
    await skip.first().click({ timeout: 5000 }).catch(() => {});
    await page.waitForTimeout(600);
  }
}

async function openServerStatus(page) {
  // 侧栏目录「系统管理」→ 子项「服务器状态」
  const group = page.locator('.t-menu__item', { hasText: '系统管理' }).first();
  if (await group.count()) {
    await group.click();
    await page.waitForTimeout(400);
  }
  await page.getByText('服务器状态', { exact: true }).first().click();
  await page.waitForURL(/\/system\/server-status/, { timeout: 15000 });
  await page.waitForSelector('.ss-hero__title', { timeout: 15000 });
}

const browser = await chromium.launch();
try {
  // ==================== 超管 ====================
  const adminCtx = await browser.newContext({ viewport: { width: 1680, height: 1050 }, deviceScaleFactor: 1 });
  // 「新功能引导」气泡是产品既有的一次性提示，会盖住右上角卡片；验收里先按真实用户
  // 行为把它标记为已看过（等价于点「知道了」），避免截图被遮挡。
  await adminCtx.addInitScript(() => {
    try {
      localStorage.setItem('cp-onboarding-done', '1');
    } catch {
      /* 隐私模式忽略 */
    }
  });
  const page = await adminCtx.newPage();
  const consoleErrors = [];
  page.on('console', (msg) => {
    if (msg.type() === 'error') consoleErrors.push(msg.text());
  });
  page.on('pageerror', (err) => consoleErrors.push(String(err)));

  await login(page, ADMIN);
  log('超管登录成功 → /dashboard');

  await openServerStatus(page);
  log(`点侧栏进入：URL=${page.url()}`);

  // 等数据到位（指标卡出现且首帧动画跑完）
  await page.waitForFunction(() => document.querySelectorAll('.ss-metric').length === 4, { timeout: 20000 });
  await page.waitForTimeout(1400);

  // ---- 四张指标卡 ----
  report.metrics = await page.$$eval('.ss-metric', (nodes) =>
    nodes.map((node) => ({
      label: node.querySelector('.ss-metric__label')?.textContent?.trim(),
      value: node.querySelector('.ss-ring__num')?.textContent?.trim(),
      unit: node.querySelector('.ss-ring__unit')?.textContent?.trim(),
      foot: node.querySelector('.ss-metric__foot')?.textContent?.trim(),
      extra: node.querySelector('.ss-metric__extra')?.textContent?.trim(),
      tone: node.querySelector('.ss-ring__value')?.className?.baseVal ?? '',
      dashoffset: node.querySelector('.ss-ring__value')?.style?.strokeDashoffset ?? '(none)',
    })),
  );
  log('指标卡：' + JSON.stringify(report.metrics));

  // ---- 环形：transition + 收敛值 ----
  report.ring = await page.evaluate(() => {
    const el = document.querySelector('.ss-ring__value');
    if (!el) return { error: '未找到环' };
    const style = getComputedStyle(el);
    const circle = el;
    const r = Number(circle.getAttribute('r'));
    const circumference = 2 * Math.PI * r;
    const offset = Number.parseFloat(style.strokeDashoffset);
    const percent = Math.round((1 - offset / circumference) * 1000) / 10;
    return {
      transitionProperty: style.transitionProperty,
      transitionDuration: style.transitionDuration,
      circumference: Math.round(circumference * 10) / 10,
      dashoffset: Math.round(offset * 10) / 10,
      impliedPercent: percent,
    };
  });
  log('环形动画：' + JSON.stringify(report.ring));

  // ---- 服务健康清单 ----
  report.health = await page.$$eval('.ss-health__row:not(.ss-health__row--head)', (rows) =>
    rows.map((row) => ({
      name: row.querySelector('.ss-health__name')?.textContent?.trim(),
      state: row.querySelector('.ss-tag')?.textContent?.trim(),
      latency: row.querySelector('.ss-health__mono')?.textContent?.trim(),
      detail: row.querySelector('.ss-health__detail')?.textContent?.trim(),
    })),
  );
  log('健康清单：' + JSON.stringify(report.health));

  // ---- 环境信息 ----
  report.env = await page.$$eval('.ss-env__card', (cards) =>
    Object.fromEntries(
      cards.map((card) => [
        card.querySelector('.ss-env__title')?.textContent?.trim() ?? '?',
        Object.fromEntries(
          Array.from(card.querySelectorAll('.ss-env__item')).map((item) => [
            item.querySelector('dt')?.textContent?.trim() ?? '?',
            item.querySelector('dd')?.textContent?.trim() ?? '',
          ]),
        ),
      ]),
    ),
  );
  log('环境信息：' + JSON.stringify(report.env));

  await page.screenshot({ path: `${OUT}/01-overview.png`, fullPage: true });
  log('首屏截图：01-overview.png');

  // 布局是「内层滚动容器」，`fullPage` 只能拍到视口高度；用高视口再拍一张完整长图
  await page.setViewportSize({ width: 1680, height: 2300 });
  await page.waitForTimeout(900);
  await page.screenshot({ path: `${OUT}/01b-overview-full.png`, fullPage: true });
  log('整页长图：01b-overview-full.png');
  await page.setViewportSize({ width: 1680, height: 1050 });
  await page.waitForTimeout(600);

  // ---- 趋势图：等第二个采样点（刷新两次，间隔 16s > 页面 15s 去抖）----
  await page.getByRole('button', { name: '刷新' }).first().click();
  await page.waitForTimeout(16000);
  await page.getByRole('button', { name: '刷新' }).first().click();
  await page.waitForTimeout(2000);
  report.charts = await page.evaluate(() => ({
    samples: document.querySelectorAll('.ss-chart__badge').length
      ? document.querySelector('.ss-chart__badge')?.textContent?.trim()
      : null,
    canvases: document.querySelectorAll('.ss-chart canvas').length,
    emptyStates: document.querySelectorAll('.ss-chart__empty').length,
  }));
  log('趋势图：' + JSON.stringify(report.charts));
  await page.locator('.ss-charts').screenshot({ path: `${OUT}/02-trends.png` });

  // ---- hover 效果（位移）----
  const card = page.locator('.ss-metric').first();
  const before = await card.evaluate((el) => getComputedStyle(el).transform);
  await card.hover();
  await page.waitForTimeout(320);
  const after = await card.evaluate((el) => getComputedStyle(el).transform);
  report.hover = { before, after };
  log('hover transform：' + JSON.stringify(report.hover));

  // ---- 下载诊断包 ----
  const [download] = await Promise.all([
    page.waitForEvent('download', { timeout: 30000 }),
    page.getByRole('button', { name: /下载诊断包/ }).click(),
  ]);
  const savePath = `${OUT}/03-diagnostic.zip`;
  await download.saveAs(savePath);
  report.download = { suggested: download.suggestedFilename(), saved: savePath };
  log('下载诊断包：' + JSON.stringify(report.download));

  report.adminConsoleErrors = consoleErrors;
  if (consoleErrors.length) log('控制台错误：' + JSON.stringify(consoleErrors));

  // ---- 窄屏响应式（<1400 / <1000）----
  await page.setViewportSize({ width: 1360, height: 1000 });
  await page.waitForTimeout(500);
  const colsAt1360 = await page.evaluate(() => getComputedStyle(document.querySelector('.ss-metrics')).gridTemplateColumns.split(' ').length);
  await page.screenshot({ path: `${OUT}/04-responsive-1360.png`, fullPage: true });
  await page.setViewportSize({ width: 960, height: 1000 });
  await page.waitForTimeout(500);
  const colsAt960 = await page.evaluate(() => getComputedStyle(document.querySelector('.ss-metrics')).gridTemplateColumns.split(' ').length);
  await page.screenshot({ path: `${OUT}/05-responsive-960.png`, fullPage: true });
  report.responsive = { colsAt1360, colsAt960 };
  log('响应式列数：' + JSON.stringify(report.responsive));

  await adminCtx.close();

  // ==================== 非超管 ====================
  const viewerCtx = await browser.newContext({ viewport: { width: 1600, height: 1000 } });
  const vpage = await viewerCtx.newPage();
  await login(vpage, VIEWER);
  log('非超管登录成功');

  const menuHit = await vpage.getByText('服务器状态', { exact: true }).count();
  report.viewer.menuHit = menuHit;
  await vpage.screenshot({ path: `${OUT}/06-viewer-dashboard.png`, fullPage: false });

  await vpage.goto(`${BASE}/system/server-status`, { waitUntil: 'domcontentloaded' });
  await vpage.waitForTimeout(1500);
  report.viewer.urlAfterDirectAccess = vpage.url();
  report.viewer.pageText = (await vpage.locator('body').innerText()).slice(0, 160).replace(/\s+/g, ' ');
  await vpage.screenshot({ path: `${OUT}/07-viewer-403.png`, fullPage: false });
  log('非超管：菜单命中=' + menuHit + '，直连后 URL=' + vpage.url());

  await viewerCtx.close();
} finally {
  await browser.close();
}

writeFileSync(`${OUT}/report.json`, JSON.stringify(report, null, 2));
console.log('\n===== REPORT =====');
console.log(JSON.stringify(report, null, 2));
