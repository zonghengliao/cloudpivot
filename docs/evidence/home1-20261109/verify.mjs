// HOME-1 真机验收：首页「公告与消息」卡（本地 dist + /api 反代 18023 真后端）
import { chromium } from '@playwright/test';
import { readFileSync } from 'node:fs';

const BASE = 'http://127.0.0.1:5301';
const D = '/root/.hermes/profiles/frontend-developer/cache/scratch/home1';
const admin = { ...JSON.parse(readFileSync(`${D}/tokens.json`, 'utf8')), username: 'admin' };
const viewer = { ...JSON.parse(readFileSync(`${D}/tokens-viewer2.json`, 'utf8')), username: 'h1viewer2' };

const out = {};
const log = (...a) => console.log(...a);

function watch(page, bucket) {
  page.on('response', (res) => {
    const u = res.url();
    if (!u.includes('/api/')) return;
    if (/announcement|notice/.test(u)) bucket.push({ status: res.status(), url: u.replace(BASE, '') });
  });
  page.on('pageerror', (e) => bucket.push({ pageerror: String(e) }));
}

async function openDashboard(browser, tokens, tag) {
  const ctx = await browser.newContext({ viewport: { width: 1600, height: 1000 } });
  await ctx.addInitScript(
    ([at, rt, uname]) => {
      localStorage.setItem('x_access_token', at);
      localStorage.setItem('x_refresh_token', rt);
      localStorage.setItem('cp-onboarding-done', '1');
      // 设置向导（SetupWizardDialog）按用户名记忆，跳过它否则遮罩会挡住列表点击
      localStorage.setItem(`cloudpivot.setup-wizard.skipped:${uname}`, '1');
      sessionStorage.setItem('login-unread-shown', '1');
    },
    [tokens.at, tokens.rt, tokens.username],
  );
  const page = await ctx.newPage();
  const bucket = [];
  watch(page, bucket);
  await page.goto(`${BASE}/dashboard`, { waitUntil: 'domcontentloaded' });
  await page.waitForSelector('.dashboard__feed .dashboard__feed-list, .dashboard__feed .dashboard__feed-empty', { timeout: 20000 });
  return { ctx, page, bucket, tag };
}

const browser = await chromium.launch();
try {
  // ---------- 场景 A：超管 ----------
  {
    const { ctx, page, bucket } = await openDashboard(browser, admin, 'admin');
    await page.waitForTimeout(500);

    const info = await page.evaluate(() => {
      const card = document.querySelector('.dashboard__feed');
      const aside = document.querySelector('.dashboard__aside');
      const secs = [...document.querySelectorAll('.dashboard__feed-section')];
      const annSec = secs.find((s) => !s.classList.contains('dashboard__feed-section--notice'));
      const msgSec = secs.find((s) => s.classList.contains('dashboard__feed-section--notice'));
      const items = (sec) => [...sec.querySelectorAll('.dashboard__feed-item')];
      const read = (el) => el?.textContent.replace(/\s+/g, ' ').trim();
      return {
        cardHeader: read(card?.querySelector('.t-card__header')),
        annCount: items(annSec).length,
        msgCount: items(msgSec).length,
        annTitles: items(annSec).map((b) => read(b.querySelector('.dashboard__feed-item-title'))),
        msgTitles: items(msgSec).map((b) => read(b.querySelector('.dashboard__feed-item-title'))),
        annTimes: items(annSec).map((b) => read(b.querySelector('.dashboard__feed-item-time'))),
        unreadDots: msgSec.querySelectorAll('.dashboard__feed-dot').length,
        unreadRows: msgSec.querySelectorAll('.dashboard__feed-item--unread').length,
        footer: read(document.querySelector('.dashboard__feed-foot')),
        asideWidth: Math.round(aside.getBoundingClientRect().width),
        asideAlign: getComputedStyle(aside).alignSelf,
        cardHeight: Math.round(card.getBoundingClientRect().height),
        skeletonNow: !!document.querySelector('.dashboard__feed-skeleton'),
        actions: read(card?.querySelector('.dashboard__feed-actions')),
        /** 首页其他区块仍在 */
        hasMetrics: document.querySelectorAll('.dashboard__metric').length,
        hasQuick: !!document.querySelector('.dashboard__quick') || !!document.querySelector('.dashboard__entries'),
        bodyText: document.body.innerText.includes('服务器状态'),
      };
    });
    out.admin = info;
    log('A 超管卡片：', JSON.stringify(info, null, 1));
    log('A 请求：', JSON.stringify(bucket, null, 1));
    await page.screenshot({ path: `${D}/shot-admin-dashboard.png`, fullPage: true });

    // 点第一条公告 → 抽屉
    const annFirst = page.locator('.dashboard__feed-section:not(.dashboard__feed-section--notice) .dashboard__feed-item').first();
    const annTitle = (await annFirst.locator('.dashboard__feed-item-title').innerText()).trim();
    await annFirst.click();
    await page.waitForSelector('.t-drawer.t-drawer--open .dashboard__detail-title', { timeout: 15000 });
    const drawer = await page.evaluate(() => {
      const d = document.querySelector('.t-drawer.t-drawer--open');
      return { title: d?.querySelector('.dashboard__detail-title')?.textContent.trim(), meta: d?.querySelector('.dashboard__detail-meta')?.textContent.replace(/\s+/g, ' ').trim() };
    });
    await page.waitForTimeout(600);
    await page.screenshot({ path: `${D}/shot-announcement-drawer.png` });
    out.announcementDrawer = { clicked: annTitle, ...drawer, requests: bucket.filter((r) => /announcement\/[0-9a-f-]{36}/.test(r.url || '')) };
    log('A 公告抽屉：', JSON.stringify(out.announcementDrawer, null, 1));

    // 关抽屉（点遮罩外坐标）后点第一条未读消息
    await page.mouse.click(160, 620);
    for (let i = 0; i < 20; i++) {
      const closed = await page.evaluate(() => !document.querySelector('.t-drawer.t-drawer--open'));
      if (closed) break;
      await page.keyboard.press('Escape');
      await page.waitForTimeout(150);
    }
    const beforeUnread = await page.evaluate(() => document.querySelectorAll('.dashboard__feed-dot').length);
    await page.locator('.dashboard__feed-section--notice .dashboard__feed-item', { hasText: '消息第12条' }).first().click();
    await page.waitForSelector('.t-drawer.t-drawer--open .dashboard__detail-title', { timeout: 15000 });
    const msgDrawer = await page.evaluate(() => {
      const d = document.querySelector('.t-drawer.t-drawer--open');
      return { title: d?.querySelector('.dashboard__detail-title')?.textContent.trim(), body: d?.querySelector('.dashboard__detail-body')?.textContent.trim() };
    });
    await page.waitForTimeout(800);
    const afterUnread = await page.evaluate(() => document.querySelectorAll('.dashboard__feed-dot').length);
    const footerAfter = await page.evaluate(() => document.querySelector('.dashboard__feed-foot')?.textContent.replace(/\s+/g, ' ').trim());
    await page.screenshot({ path: `${D}/shot-notice-drawer.png` });
    out.noticeDrawer = { ...msgDrawer, beforeUnread, afterUnread, footerAfter, requests: bucket.filter((r) => /notice\/read/.test(r.url || '')) };
    log('A 消息抽屉：', JSON.stringify(out.noticeDrawer, null, 1));
    await page.screenshot({ path: `${D}/shot-notice-drawer.png` });

    // 右上角两个入口（「全部」）必须真的跳走：公告 → /announcement，消息 → /business/notice/message
    await page.mouse.click(160, 620);
    for (let i = 0; i < 20; i++) {
      if (await page.evaluate(() => !document.querySelector('.t-drawer.t-drawer--open'))) break;
      await page.keyboard.press('Escape');
      await page.waitForTimeout(150);
    }
    await page.getByText('公告全部', { exact: true }).click();
    await page.waitForTimeout(1200);
    const annUrl = new URL(page.url()).pathname;
    await page.goBack({ waitUntil: 'domcontentloaded' });
    await page.waitForSelector('.dashboard__feed-item', { timeout: 20000 });
    await page.getByText('消息全部', { exact: true }).click();
    await page.waitForTimeout(1200);
    const msgUrl = new URL(page.url()).pathname;
    out.navigation = { annUrl, msgUrl };
    log('A 入口跳转：', JSON.stringify(out.navigation));
    await ctx.close();
  }

  // ---------- 场景 B：非超管（DEPT_LEADER，0 条消息 → 真机空态） ----------
  {
    const { ctx, page, bucket } = await openDashboard(browser, viewer, 'viewer');
    await page.waitForTimeout(500);
    const info = await page.evaluate(() => {
      const card = document.querySelector('.dashboard__feed');
      const secs = [...document.querySelectorAll('.dashboard__feed-section')];
      const annSec = secs.find((s) => !s.classList.contains('dashboard__feed-section--notice'));
      const msgSec = secs.find((s) => s.classList.contains('dashboard__feed-section--notice'));
      return {
        cardVisible: !!card && card.getBoundingClientRect().height > 200,
        cardHeader: card?.querySelector('.t-card__header')?.textContent.replace(/\s+/g, ' ').trim(),
        annCount: annSec.querySelectorAll('.dashboard__feed-item').length,
        msgEmpty: msgSec.querySelector('.dashboard__feed-empty')?.textContent.replace(/\s+/g, ' ').trim(),
        footer: document.querySelector('.dashboard__feed-foot')?.textContent.replace(/\s+/g, ' ').trim(),
      };
    });
    out.viewer = { ...info, requests: bucket };
    log('B 非超管：', JSON.stringify(info, null, 1));
    log('B 请求：', JSON.stringify(bucket, null, 1));
    await page.screenshot({ path: `${D}/shot-viewer-dashboard.png`, fullPage: true });
    await ctx.close();
  }

  // ---------- 场景 C：两个接口都返回空（拦截层，验空态 + 骨架屏） ----------
  {
    const ctx = await browser.newContext({ viewport: { width: 1600, height: 1000 } });
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
    const empty = JSON.stringify({ code: 0, msg: 'ok', data: { total: 0, page: 1, page_size: 10, list: [] } });
    // 延迟 1.5s 才返回，用于抓骨架屏
    await page.route(/\/api\/business\/(announcement\/visible|notice\/mine)/, async (route) => {
      await new Promise((r) => setTimeout(r, 1500));
      await route.fulfill({ status: 200, headers: { 'content-type': 'application/json' }, body: empty });
    });
    await page.goto(`${BASE}/dashboard`, { waitUntil: 'domcontentloaded' });
    await page.waitForSelector('.dashboard__feed-skeleton', { timeout: 10000 });
    const skeleton = await page.evaluate(() => ({ rows: document.querySelectorAll('.dashboard__feed-skeleton-row').length, visible: !!document.querySelector('.dashboard__feed-skeleton')?.getBoundingClientRect().height }));
    await page.screenshot({ path: `${D}/shot-skeleton.png` });
    await page.waitForSelector('.dashboard__feed-empty', { timeout: 15000 });
    const emptyState = await page.evaluate(() => ({
      texts: [...document.querySelectorAll('.dashboard__feed .dashboard__feed-empty')].map((e) => e.textContent.replace(/\s+/g, ' ').trim()),
      footer: document.querySelector('.dashboard__feed-foot')?.textContent.replace(/\s+/g, ' ').trim(),
      skeletonGone: !document.querySelector('.dashboard__feed-skeleton'),
    }));
    await page.waitForTimeout(300);
    await page.screenshot({ path: `${D}/shot-empty.png`, fullPage: true });
    out.empty = { skeleton, emptyState };
    log('C 骨架/空态：', JSON.stringify(out.empty, null, 1));
    await ctx.close();
  }
} finally {
  await browser.close();
}

import { writeFileSync } from 'node:fs';
writeFileSync(`${D}/verify-result.json`, JSON.stringify(out, null, 2));
log('\n结果已写 verify-result.json');
