/**
 * t_cea00ecf 红证据：同一份脚本对着**修复前**的 BatchActionBar（git HEAD 版本）跑批量导出，
 * 只验一条：「勾选 → 批量导出 → 结果弹窗出现 / 工具栏还在吗」。
 * 期望（缺陷态）：请求 200 + task_no 有值，但弹窗 dialogShown=false、工具栏已卸载。
 */
import { chromium } from '@playwright/test';
import fs from 'node:fs';

const BASE = 'http://127.0.0.1:5212';
const API = 'http://127.0.0.1:18126';
const OUT = '/root/cea00ecf-wt/docs/evidence/20261105-batch-result-dialog';
fs.mkdirSync(OUT, { recursive: true });

const login = await (
  await fetch(`${API}/api/system/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username: 'admin', password: 'Admin@123456' }),
  })
).json();

const browser = await chromium.launch();
const context = await browser.newContext({ viewport: { width: 1600, height: 1000 } });
await context.addInitScript(
  ([a, r]) => {
    localStorage.setItem('x_access_token', a);
    localStorage.setItem('x_refresh_token', r);
    localStorage.setItem('cp-onboarding-done', '1');
    sessionStorage.setItem('login-unread-shown', '1');
  },
  [login.data.access_token, login.data.refresh_token]
);
const page = await context.newPage();
const requests = [];
page.on('response', async (resp) => {
  if (resp.url().includes('/batch-export')) {
    let body = null;
    try { body = await resp.json(); } catch { /* ignore */ }
    requests.push({ url: resp.url(), status: resp.status(), task_no: body?.data?.task_no, total: body?.data?.total });
  }
});

const result = {};
try {
  await page.goto(`${BASE}/business/erp/payable`, { waitUntil: 'domcontentloaded' });
  await page.waitForSelector('[data-testid="payable-table"] tbody tr', { timeout: 30000 });
  await page.waitForFunction(
    () => /共\s*[1-9]\d*\s*条/.test(document.querySelector('[data-testid="payable-meta"]')?.textContent ?? ''),
    null,
    { timeout: 20000 }
  );
  await page.locator('[data-testid="payable-table"] tbody tr').first().locator('.t-checkbox').first().click();
  await page.waitForSelector('[data-testid="payable-batch"]', { state: 'visible', timeout: 10000 });
  await page.locator('[data-testid="payable-batch-export"]').click();
  // 等请求落地 + 两拍渲染
  await page.waitForFunction(() => performance.getEntriesByType('resource').some((e) => e.name.includes('/batch-export')), null, { timeout: 20000 }).catch(() => {});
  await page.waitForTimeout(1500);
  result.dialogShown = await page
    .evaluate(() => {
      const ctx = document.querySelector('[data-testid="payable-batch-result"]');
      const dlg = ctx?.querySelector('.t-dialog');
      return !!dlg && dlg.offsetParent !== null;
    });
  result.barMounted = (await page.locator('[data-testid="payable-batch"]').count()) > 0;
  result.barVisible = await page.locator('[data-testid="payable-batch"]').isVisible().catch(() => false);
  result.messageToast = await page.evaluate(() =>
    [...document.querySelectorAll('.t-message')].map((el) => el.innerText.trim())
  );
  result.requests = requests;
  await page.screenshot({ path: `${OUT}/R-before-fix-no-dialog.png` });
} catch (err) {
  result.error = String(err?.stack ?? err);
} finally {
  console.log(JSON.stringify(result, null, 2));
  fs.writeFileSync(`${OUT}/report-before-fix.json`, JSON.stringify(result, null, 2));
  await browser.close();
}
