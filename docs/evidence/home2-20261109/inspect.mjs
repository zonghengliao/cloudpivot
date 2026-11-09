// 细看工作任务卡：元素截图 + 每行截断/间距/高度实测
import { chromium } from '@playwright/test';
import { readFileSync } from 'node:fs';

const BASE = 'http://127.0.0.1:5302';
const D = '/root/.hermes/profiles/frontend-developer/cache/scratch/home2';
const tokens = { ...JSON.parse(readFileSync(`${D}/tokens.json`, 'utf8')), username: 'admin' };

const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1600, height: 1200 } });
await ctx.addInitScript(
  ([at, rt]) => {
    localStorage.setItem('x_access_token', at);
    localStorage.setItem('x_refresh_token', rt);
    localStorage.setItem('cp-onboarding-done', '1');
    localStorage.setItem('cloudpivot.setup-wizard.skipped:admin', '1');
    sessionStorage.setItem('login-unread-shown', '1');
  },
  [tokens.at, tokens.rt],
);
const page = await ctx.newPage();
await page.goto(`${BASE}/dashboard`, { waitUntil: 'domcontentloaded' });
await page.waitForSelector('.dashboard__tasks-item', { timeout: 15000 });
await page.waitForTimeout(500);
await page.locator('.dashboard__aside').scrollIntoViewIfNeeded();

const measure = await page.evaluate(() => {
  const card = document.querySelector('.dashboard__tasks');
  const rows = [...card.querySelectorAll('.dashboard__tasks-item')];
  return {
    cardHeight: Math.round(card.getBoundingClientRect().height),
    cardTop: Math.round(card.getBoundingClientRect().top),
    feedBottom: Math.round(document.querySelectorAll('.dashboard__aside .t-card')[0].getBoundingClientRect().bottom),
    gap: Math.round(card.getBoundingClientRect().top - document.querySelectorAll('.dashboard__aside .t-card')[0].getBoundingClientRect().bottom),
    rows: rows.map((r) => {
      const t = r.querySelector('.dashboard__tasks-title');
      const m = r.querySelector('.dashboard__tasks-main');
      return {
        h: Math.round(r.getBoundingClientRect().height),
        titleW: Math.round(t.getBoundingClientRect().width),
        titleScrollW: t.scrollWidth,
        titleClientW: t.clientWidth,
        overflow: t.scrollWidth > t.clientWidth,
        mainW: Math.round(m.getBoundingClientRect().width),
        text: t.textContent.trim().slice(0, 14),
      };
    }),
  };
});
console.log(JSON.stringify(measure, null, 2));

const box = await page.locator('.dashboard__tasks').boundingBox();
await page.screenshot({ path: `${D}/shot-tasks-card.png`, clip: box });
await page.screenshot({ path: `${D}/shot-dashboard-full.png`, fullPage: true });
await browser.close();
