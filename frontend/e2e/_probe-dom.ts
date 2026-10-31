/** 临时探针：确认表格行选择器、弹窗可见性、空提交后的校验 DOM。跑法：npx tsx e2e/_probe-dom.ts */
import { createRequire } from 'node:module';
import { readFileSync } from 'node:fs';

const require = createRequire('/opt/cloudpivot/frontend/package.json');
const { chromium } = require('@playwright/test');

const BASE = process.env.E2E_BASE_URL ?? 'http://172.18.10.144';
const token = readFileSync('/opt/cloudpivot/frontend/e2e/.auth/token.txt', 'utf8').trim();
const browser = await chromium.launch();
const ctx = await browser.newContext({
  baseURL: BASE,
  storageState: {
    cookies: [],
    origins: [
      {
        origin: BASE,
        localStorage: [
          { name: 'x_access_token', value: token },
          { name: 'cp-onboarding-done', value: '1' },
        ],
      },
    ],
  },
  viewport: { width: 1600, height: 1000 },
  locale: 'zh-CN',
});
const page = await ctx.newPage();
await page.goto('/business/erp/material', { waitUntil: 'domcontentloaded' });
await page.waitForSelector('.basic-layout__aside');
await page.waitForFunction(() => document.querySelectorAll('.t-table').length > 0, undefined, { timeout: 20000 });
await page.waitForLoadState('networkidle', { timeout: 8000 }).catch(() => undefined);

const tableInfo = await page.evaluate(() => {
  const t = document.querySelector('.t-table')!;
  const tr = t.querySelectorAll('tbody tr');
  return {
    trCount: tr.length,
    firstTrClass: tr[0]?.className ?? '',
    firstTrHtml: (tr[0]?.outerHTML ?? '').slice(0, 400),
    rowSelectors: {
      'tbody tr': t.querySelectorAll('tbody tr').length,
      'tr.t-table__row': t.querySelectorAll('tr.t-table__row').length,
      '.t-table__row': t.querySelectorAll('.t-table__row').length,
    },
  };
});
console.log('TABLE', JSON.stringify(tableInfo, null, 1));

// 弹窗可见性
const dlg = await page.evaluate(() => {
  const d = document.querySelector('.t-dialog');
  if (!d) return { exists: false };
  const r = d.getBoundingClientRect();
  return {
    exists: true,
    cls: d.className,
    rect: [r.width, r.height],
    display: getComputedStyle(d).display,
    parentCls: d.parentElement?.className ?? '',
    visibleDialogs: Array.from(document.querySelectorAll('.t-dialog')).filter((el) => (el as HTMLElement).offsetParent !== null).length,
  };
});
console.log('DIALOG_BEFORE', JSON.stringify(dlg));

await page.locator('.basic-layout__content button', { hasText: '新建物料' }).first().click();
await page.waitForTimeout(600);
const opened = await page.evaluate(() => {
  const vis = Array.from(document.querySelectorAll('.t-dialog')).filter((el) => (el as HTMLElement).offsetParent !== null);
  return { visibleDialogs: vis.length, cls: vis[0]?.className ?? '', text: (vis[0]?.textContent ?? '').replace(/\s+/g, ' ').slice(0, 200) };
});
console.log('DIALOG_OPEN', JSON.stringify(opened));

const reqs: string[] = [];
page.on('request', (r) => {
  if (r.method() !== 'GET' && r.url().includes('/api/')) reqs.push(`${r.method()} ${r.url()}`);
});

await page.locator('.t-dialog:visible button', { hasText: /^创建$/ }).first().click();
await page.waitForTimeout(1200);
const validation = await page.evaluate(() => {
  const vis = Array.from(document.querySelectorAll('.t-dialog')).filter((el) => (el as HTMLElement).offsetParent !== null)[0];
  const scope = vis ?? document.body;
  const errish = Array.from(scope.querySelectorAll('[class*="error"], [class*="extra"], [class*="help"]')).map((el) => ({
    cls: el.className.slice(0, 100),
    text: (el.textContent ?? '').replace(/\s+/g, ' ').trim().slice(0, 80),
  }));
  return {
    dialogStillOpen: Boolean(vis),
    errish: errish.slice(0, 12),
    inputCls: Array.from(scope.querySelectorAll('.t-input')).slice(0, 3).map((el) => el.className.slice(0, 120)),
    toasts: Array.from(document.querySelectorAll('.t-message')).map((el) => (el.textContent ?? '').trim()),
    bodyText: (scope.textContent ?? '').replace(/\s+/g, ' ').slice(0, 300),
  };
});
console.log('AFTER_EMPTY_SUBMIT', JSON.stringify(validation, null, 1));
console.log('WRITE_REQS', JSON.stringify(reqs));
await browser.close();
