// HOME-2 真机验收：首页「工作任务」卡（本地 dist + /api 反代 18023 真后端）
import { chromium } from '@playwright/test';
import { readFileSync, writeFileSync } from 'node:fs';

const BASE = 'http://127.0.0.1:5302';
const D = '/root/.hermes/profiles/frontend-developer/cache/scratch/home2';
const admin = { ...JSON.parse(readFileSync(`${D}/tokens.json`, 'utf8')), username: 'admin' };
const viewer = { ...JSON.parse(readFileSync(`${D}/login-viewer2.json`, 'utf8')).data, username: 'h1viewer2' };
const viewerTokens = { at: viewer.access_token, rt: viewer.refresh_token };

const out = { base: BASE, check: {}, api: [] };
const log = (...a) => console.log(...a);
const save = () => writeFileSync(`${D}/verify-result.json`, JSON.stringify(out, null, 2));

async function openDashboard(browser, tokens, tag, opts = {}) {
  const ctx = await browser.newContext({ viewport: { width: 1600, height: 1400 } });
  await ctx.addInitScript(
    ([at, rt, uname]) => {
      localStorage.setItem('x_access_token', at);
      localStorage.setItem('x_refresh_token', rt);
      localStorage.setItem('cp-onboarding-done', '1');
      localStorage.setItem(`cloudpivot.setup-wizard.skipped:${uname}`, '1');
      // 登录后「你有 N 条待处理」汇总弹窗会挡住点击；验收页不需要它
      sessionStorage.setItem('login-unread-shown', '1');
    },
    [tokens.at, tokens.rt, tokens.username || tag],
  );
  const page = await ctx.newPage();
  page.on('response', (res) => {
    const u = res.url();
    if (u.includes('/api/') && /task/.test(u)) out.api.push({ tag, status: res.status(), url: u.replace(BASE, '') });
  });
  page.on('pageerror', (e) => out.api.push({ tag, pageerror: String(e) }));
  if (opts.delayTaskMy) {
    await page.route('**/api/business/task/my*', async (route) => {
      await new Promise((r) => setTimeout(r, opts.delayTaskMy));
      await route.continue();
    });
  }
  await page.goto(`${BASE}/dashboard`, { waitUntil: 'domcontentloaded' });
  return { ctx, page };
}

const browser = await chromium.launch();

// ---------- A. 超管：有 12 条任务 ----------
{
  const { ctx, page } = await openDashboard(browser, admin, 'admin');
  if (out.check.skeleton_rows_at_first_paint === undefined) out.check.skeleton_rows_at_first_paint = 0;

  // 骨架：先抓一次（接口未返回前）
  const skeletonSeen = await page
    .waitForSelector('.dashboard__tasks-skeleton-row', { timeout: 3000 })
    .then(() => page.locator('.dashboard__tasks-skeleton-row').count())
    .catch(() => 0);
  out.check.skeleton_rows = skeletonSeen;

  await page.waitForSelector('.dashboard__tasks .dashboard__tasks-item', { timeout: 15000 });
  await page.waitForTimeout(600);

  const cardOrder = await page.$$eval('.dashboard__aside .t-card', (cards) =>
    cards.map((c) => c.querySelector('.t-card__title, .t-card-header__title, [class*="title"]')?.textContent?.trim() || ''),
  );
  out.check.aside_card_titles = cardOrder;

  const info = await page.evaluate(() => {
    const card = document.querySelector('.dashboard__tasks');
    const rows = [...card.querySelectorAll('.dashboard__tasks-item')];
    const due = rows.map((r) => r.querySelector('.dashboard__tasks-due'));
    return {
      rowCount: rows.length,
      titles: rows.map((r) => r.querySelector('.dashboard__tasks-title')?.textContent?.trim()),
      dues: due.map((d) => d.textContent.trim()),
      overdueRows: rows.filter((r) => r.querySelector('.dashboard__tasks-due--overdue')).length,
      overdueColors: due.map((d) => getComputedStyle(d).color),
      normalDueColor: (() => {
        const n = rows.find((r) => !r.querySelector('.dashboard__tasks-due--overdue'));
        return n ? getComputedStyle(n.querySelector('.dashboard__tasks-due')).color : null;
      })(),
      tagTexts: rows.map((r) => r.querySelector('.dashboard__tasks-tag')?.textContent?.trim() || null),
      doingRows: rows.filter((r) => r.querySelector('.dashboard__tasks-state--doing')).length,
      doingBg: (() => {
        const el = rows.find((r) => r.querySelector('.dashboard__tasks-state--doing'))?.querySelector('.dashboard__tasks-state--doing');
        return el ? getComputedStyle(el).backgroundColor : null;
      })(),
      cardHeight: Math.round(card.getBoundingClientRect().height),
      asideWidth: Math.round(document.querySelector('.dashboard__aside')?.getBoundingClientRect().width || 0),
      footText: card.querySelector('.dashboard__tasks-foot')?.textContent?.replace(/\s+/g, ' ').trim(),
      emptyVisible: !!card.querySelector('.dashboard__tasks-empty'),
      longTitle: (() => {
        const els = [...card.querySelectorAll('.dashboard__tasks-title')];
        const overflowing = els.filter((el) => el.scrollWidth > el.clientWidth);
        const el = overflowing[0] || els.sort((a, b) => b.textContent.length - a.textContent.length)[0];
        return el
          ? {
              text: el.textContent.trim(),
              titleAttr: el.getAttribute('title'),
              truncated: el.scrollWidth > el.clientWidth,
              scrollWidth: el.scrollWidth,
              clientWidth: el.clientWidth,
              overflowingRows: overflowing.length,
            }
          : null;
      })(),
    };
  });
  out.check.admin = info;

  await page.screenshot({ path: `${D}/shot-admin-tasks.png`, fullPage: false });
  await page.screenshot({ path: `${D}/shot-dashboard-full.png`, fullPage: true });
  await page.locator('.dashboard__tasks').screenshot({ path: `${D}/shot-tasks-card.png` });

  // 点第一条 → 任务列表 + 详情抽屉
  const firstTitle = info.titles[0];
  await page.locator('.dashboard__tasks-item').first().click();
  await page.waitForURL(/\/task\/list/, { timeout: 10000 });
  await page.waitForSelector('.t-drawer', { timeout: 10000 });
  await page.waitForTimeout(800);
  out.check.navigation = {
    url: page.url().replace(BASE, ''),
    clicked: firstTitle,
    drawerHeader: (await page.locator('.t-drawer__header').first().innerText().catch(() => '')).replace(/\s+/g, ' ').trim(),
    drawerVisible: await page.locator('.t-drawer').first().isVisible(),
  };
  await page.screenshot({ path: `${D}/shot-task-detail-from-card.png`, fullPage: false });
  await ctx.close();
}

// ---------- B. 骨架屏（延迟 1.5s） ----------
{
  const { ctx, page } = await openDashboard(browser, admin, 'admin-skeleton', { delayTaskMy: 1500 });
  await page.waitForSelector('.dashboard__tasks-skeleton-row', { timeout: 5000 }).catch(() => {});
  out.check.skeleton_rows = await page.locator('.dashboard__tasks-skeleton-row').count();
  await page.screenshot({ path: `${D}/shot-skeleton.png`, clip: await page.locator('.dashboard__aside').boundingBox().catch(() => null) });
  await page.waitForSelector('.dashboard__tasks-item', { timeout: 15000 }).catch(() => {});
  out.check.skeleton_left_after_load = await page.locator('.dashboard__tasks-skeleton-row').count();
  await ctx.close();
}

// ---------- C. 非超管 viewer2：真机空态 ----------
{
  const { ctx, page } = await openDashboard(browser, viewerTokens, 'h1viewer2');
  await page.waitForSelector('.dashboard__tasks .dashboard__tasks-empty', { timeout: 15000 });
  const empty = await page.evaluate(() => {
    const el = document.querySelector('.dashboard__tasks-empty');
    const btn = [...document.querySelectorAll('.dashboard__tasks-empty button')];
    return {
      text: el.textContent.replace(/\s+/g, ' ').trim(),
      buttonTexts: btn.map((b) => b.textContent.trim()),
      cardVisible: !!document.querySelector('.dashboard__tasks'),
      footText: document.querySelector('.dashboard__tasks-foot')?.textContent?.replace(/\s+/g, ' ').trim() || null,
      errorBar: !!document.querySelector('.dashboard__tasks-error'),
    };
  });
  out.check.empty = empty;
  await page.screenshot({ path: `${D}/shot-empty-viewer.png`, fullPage: false });
  // 点「去任务中心」
  const btn = page.locator('.dashboard__tasks-empty button').first();
  if (await btn.isVisible().catch(() => false)) {
    await btn.click();
    await page.waitForURL(/\/task\/list/, { timeout: 10000 });
    out.check.empty_button_nav = page.url().replace(BASE, '');
  }
  await ctx.close();
}

// ---------- D. 已完成样式（拦截层构造一条 DONE：`/task/my` 本身不返回终态） ----------
{
  const ctx = await browser.newContext({ viewport: { width: 1600, height: 1400 } });
  await ctx.addInitScript(
    ([at, rt]) => {
      localStorage.setItem('x_access_token', at);
      localStorage.setItem('x_refresh_token', rt);
      localStorage.setItem('cp-onboarding-done', '1');
      localStorage.setItem('cloudpivot.setup-wizard.skipped:admin', '1');
      sessionStorage.setItem('login-unread-shown', '1');
    },
    [admin.at, admin.rt],
  );
  const page = await ctx.newPage();
  await page.route('**/api/business/task/my', async (route) => {
    const res = await route.fetch();
    const body = await res.json();
    if (body?.data?.today?.length) {
      body.data.today[0] = { ...body.data.today[0], status: 'DONE', title: '【样式验证】完成入库单核对（已办）' };
    }
    await route.fulfill({ response: res, json: body });
  });
  await page.goto(`${BASE}/dashboard`, { waitUntil: 'domcontentloaded' });
  await page.waitForSelector('.dashboard__tasks-item', { timeout: 15000 });
  await page.waitForTimeout(400);
  out.check.done_row = await page.evaluate(() => {
    const row = [...document.querySelectorAll('.dashboard__tasks-item')].find((r) => r.classList.contains('dashboard__tasks-item--done'));
    if (!row) return null;
    const title = row.querySelector('.dashboard__tasks-title');
    const state = row.querySelector('.dashboard__tasks-state');
    return {
      title: title.textContent.trim(),
      textDecoration: getComputedStyle(title).textDecorationLine,
      titleColor: getComputedStyle(title).color,
      stateHasCheckIcon: !!state.querySelector('svg'),
      stateBg: getComputedStyle(state).backgroundColor,
      dueText: row.querySelector('.dashboard__tasks-due').textContent.trim(),
    };
  });
  await page.locator('.dashboard__tasks').screenshot({ path: `${D}/shot-done-row-real.png` });
  await ctx.close();
}

await browser.close();
save();
console.log(JSON.stringify(out, null, 2));
