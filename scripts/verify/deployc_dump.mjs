/**
 * DEPLOY-C 证据补充：把「结账弹窗 / 年结页」的挡路单据明细表格**原文**打出来（截图之外的可核对文本）。
 * 用法：cd /opt/cloudpivot/frontend && DC_PASS=... node scripts/verify/deployc_dump.mjs
 */
import { writeFileSync, mkdirSync } from 'node:fs';
import { createRequire } from 'node:module';

const FE_DIR = process.env.DC_FE_DIR ?? '/opt/cloudpivot/frontend';
const { chromium } = createRequire(`${FE_DIR}/package.json`)('@playwright/test');
const BASE = process.env.DC_BASE ?? 'http://127.0.0.1';
const OUT = process.env.DC_OUT ?? '/root/dc-evidence/run5';
const USER = process.env.DC_USER ?? 'admin';
const PASS = process.env.DC_PASS ?? '';
const PERIOD = process.env.DC_PERIOD ?? '2026-10';
const YEAR = process.env.DC_YEAR ?? '2026';
mkdirSync(OUT, { recursive: true });

const browser = await chromium.launch({ args: ['--no-sandbox'] });
const page = await (await browser.newContext({ viewport: { width: 1600, height: 1000 } })).newPage();
const out = {};

await page.goto(`${BASE}/login`, { waitUntil: 'domcontentloaded' });
await page.fill('input[placeholder="请输入用户名"]', USER);
await page.fill('input[placeholder="请输入密码"]', PASS);
await page.click('button:has-text("登")');
await page.waitForURL(/dashboard/, { timeout: 30000 });
await page.waitForTimeout(1500);

// ① 结账弹窗里的挡路单据明细表
await page.goto(`${BASE}/business/erp/period`, { waitUntil: 'domcontentloaded' });
await page.waitForSelector('[data-testid="period-table"]', { timeout: 30000 });
await page.locator(`[data-testid="btn-period-close-${PERIOD}"]`).dispatchEvent('click');
await page.waitForSelector('[data-testid^="period-blocker-table-"]', { timeout: 30000 });
await page.waitForTimeout(1500);
out.period = PERIOD;
out.periodCloseBlockedText = (await page.locator('[data-testid^="period-blocker-table-"]').first().innerText()).slice(0, 4000);
out.periodDialogText = (await page.locator('.t-dialog').first().innerText()).slice(0, 4000);

// ② 年结页的明细清单
await page.goto(`${BASE}/business/erp/year-end?year=${YEAR}`, { waitUntil: 'domcontentloaded' });
await page.waitForSelector('[data-testid="yearend-check-list"]', { timeout: 30000 });
await page.waitForTimeout(1500);
out.year = YEAR;
out.yearEndCheckText = (await page.locator('[data-testid="yearend-check-list"]').innerText()).slice(0, 3000);
const yeTable = page.locator('[data-testid^="yearend-blocker-table-"]');
out.yearEndBlockedCount = await yeTable.count();
if (out.yearEndBlockedCount > 0) {
  out.yearEndBlockedText = (await yeTable.first().innerText()).slice(0, 4000);
}

writeFileSync(`${OUT}/page-text.json`, JSON.stringify(out, null, 2));
console.log(JSON.stringify(out, null, 2));
await browser.close();
