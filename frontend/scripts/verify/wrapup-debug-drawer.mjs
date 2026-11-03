/**
 * 调试用：打开生产采购单详情抽屉，dump 抽屉文本 + 网络失败，判断「审批进度」区块为什么不出现。
 * 用法：WU_PASS=<env 口令> node scripts/verify/wrapup-debug-drawer.mjs
 */
import { chromium } from '@playwright/test';

const BASE = (process.env.WU_BASE ?? 'http://172.18.10.144').replace(/\/$/, '');
const USER = process.env.WU_USER ?? 'admin';
const PASS = process.env.WU_PASS ?? '';

const browser = await chromium.launch();
const page = await (await browser.newContext({ viewport: { width: 1600, height: 1000 } })).newPage();

const netFail = [];
page.on('response', (r) => {
  const u = r.url();
  if (u.includes('/api/') && r.status() >= 400) netFail.push(`${r.status()} ${u}`);
});
page.on('requestfailed', (r) => {
  if (r.url().includes('/api/')) netFail.push(`NETFAIL ${r.url()} ${r.failure()?.errorText}`);
});

await page.goto(`${BASE}/login`, { waitUntil: 'domcontentloaded' });
await page.waitForSelector("input[placeholder='请输入用户名']", { timeout: 30000 });
await page.fill("input[placeholder='请输入用户名']", USER);
await page.fill("input[placeholder='请输入密码']", PASS);
await page.click('button.t-button--theme-primary');
await page.waitForURL(/\/(dashboard|change-password)/, { timeout: 40000 });

await page.goto(`${BASE}/business/erp/purchase-order`, { waitUntil: 'domcontentloaded' });
await page.waitForSelector('[data-testid="purchase-table"] tbody tr', { timeout: 30000 });
const rowsText = await page.locator('[data-testid="purchase-table"] tbody tr').allInnerTexts();
console.log('=== 前 5 行 ===');
rowsText.slice(0, 5).forEach((t, i) => console.log(`[${i + 1}] ${t.replace(/\n+/g, ' | ').slice(0, 120)}`));

await page.locator('[data-testid="purchase-table"] tbody tr').first().click();
await page.waitForSelector('[data-testid="purchase-detail-drawer"]', { timeout: 20000 });
for (const wait of [2000, 5000, 10000]) {
  await page.waitForTimeout(wait);
  const txt = await page
    .locator('[data-testid="purchase-detail-drawer"]')
    .innerText()
    .catch((e) => `ERR ${e}`);
  const loading = await page.locator('[data-testid="purchase-detail-drawer"] .drawer-loading').count();
  console.log(`=== 抽屉文本 @+${wait}ms (loading=${loading}) ===`);
  console.log(txt.replace(/\n{2,}/g, '\n').slice(0, 800));
}
console.log('=== API 失败 ===');
console.log(netFail.length ? netFail.join('\n') : '(无)');
await page.screenshot({ path: 'docs/evidence/20261103-wrapup/shots/debug-drawer.png' });
await browser.close();
