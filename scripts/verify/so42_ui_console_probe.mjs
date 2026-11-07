/** 定位 console.error 来源页（SO-42 收尾用，跑完即弃） */
import { chromium } from '@playwright/test';

const BASE = 'http://127.0.0.1:5188';

const main = async () => {
  const res = await fetch(`${BASE}/api/system/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username: 'admin', password: 'Admin@123456' }),
  });
  const tokens = (await res.json()).data;
  const browser = await chromium.launch();
  const context = await browser.newContext({ viewport: { width: 1680, height: 1000 } });
  await context.addInitScript(
    ([a, r]) => {
      localStorage.setItem('x_access_token', a);
      localStorage.setItem('x_refresh_token', r);
      localStorage.setItem('cp-onboarding-done', '1');
      sessionStorage.setItem('login-unread-shown', '1');
    },
    [tokens.access_token, tokens.refresh_token],
  );
  const page = await context.newPage();
  let current = '';
  page.on('console', (msg) => {
    if (msg.type() === 'error') console.log(`[${current}] ${msg.text().slice(0, 300)}`);
  });
  page.on('pageerror', (err) => console.log(`[${current}] PAGEERROR ${err.message.slice(0, 300)}`));

  const pages = [
    '/business/erp/currency',
    '/business/erp/exchange-rate',
    '/business/erp/sales-order',
  ];
  for (const url of pages) {
    current = url;
    console.log(`---- ${url}`);
    await page.goto(BASE + url);
    await page.waitForTimeout(4000);
    if (url.includes('exchange-rate')) {
      await page.locator('[data-testid="btn-rate-batch"]').click();
      await page.waitForTimeout(2500);
      await page.keyboard.press('Escape');
    }
  }
  await browser.close();
};

main();
