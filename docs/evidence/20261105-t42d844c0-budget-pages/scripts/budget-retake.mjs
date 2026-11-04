/**
 * 补拍（不改数据）：列表首屏（等图标加载完）+ 映射弹窗（hint 改成 form-item help 后）+ MATERIAL_TYPE 拦截提示。
 */
import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';

const BASE = 'http://127.0.0.1:5180';
const API = 'http://127.0.0.1:18097';
const OUT = '/root/.hermes/profiles/frontend-developer/cache/scratch/budget-shots';

const loginBody = await (await fetch(`${API}/api/system/auth/login`, {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ username: 'admin', password: 'Manual@123456' }),
})).json();
const { access_token: token, refresh_token: refresh } = loginBody.data;

const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, locale: 'zh-CN' });
await ctx.addInitScript(
  ([a, r]) => {
    localStorage.setItem('x_access_token', a);
    localStorage.setItem('x_refresh_token', r);
    localStorage.setItem('x_guide_done', '1');
    localStorage.setItem('cp-onboarding-done', '1');
    localStorage.setItem('cp-guide-dismissed', '1');
    sessionStorage.setItem('login-unread-shown', '1');
  },
  [token, refresh],
);
const page = await ctx.newPage();
page.setDefaultTimeout(20000);
const errors = [];
page.on('response', (r) => {
  if (r.status() >= 400 && r.url().includes('/api/') && !r.url().includes('/notice/stream')) {
    errors.push(`${r.status()} ${r.url().replace(BASE, '').split('?')[0]}`);
  }
});

async function closeUnread() {
  const btn = page.getByTestId('login-unread-ok');
  if (await btn.count()) {
    await btn.click().catch(() => {});
    await page.waitForTimeout(400);
  }
}

// ① 列表首屏（等 3s 让侧栏图标 chunk 落地）
await page.goto(`${BASE}/business/erp/budget`, { waitUntil: 'domcontentloaded' });
await page.waitForTimeout(3000);
await closeUnread();
await page.waitForTimeout(800);
await page.screenshot({ path: `${OUT}/01-budget-list.png` });
console.log('[shot] 01-budget-list.png');

// ② 映射弹窗（hint 变成 form-item help 后）
await page.goto(`${BASE}/business/erp/budget/mapping`, { waitUntil: 'domcontentloaded' });
await page.waitForTimeout(2500);
await closeUnread();
await page.getByTestId('budget-mapping-btn-create').click();
await page.waitForTimeout(700);
await page.getByTestId('budget-mapping-form-account').click();
await page.waitForTimeout(400);
await page.locator('.t-select-option:visible').nth(1).click();
await page.waitForTimeout(400);
await page.screenshot({ path: `${OUT}/08-budget-mapping-dialog.png` });
console.log('[shot] 08-budget-mapping-dialog.png');

// ③ MATERIAL_TYPE 拦截提示（后端已知缺陷的前端拦截面）
await page.getByTestId('budget-mapping-form-type').click();
await page.waitForTimeout(400);
await page.locator('.t-select-option:visible').nth(1).click();
await page.waitForTimeout(600);
const blocker = (await page.getByTestId('budget-mapping-blocker').innerText().catch(() => '')).trim();
const saveDisabled = await page
  .locator('.t-dialog__footer .t-button--theme-primary')
  .first()
  .getAttribute('disabled');
console.log(`[blocker] ${blocker.slice(0, 60)}… saveDisabled=${saveDisabled !== null}`);
await page.screenshot({ path: `${OUT}/11-budget-mapping-blocker.png` });
console.log('[shot] 11-budget-mapping-blocker.png');

console.log('[api-errors]', errors.length ? errors : 'none');
await browser.close();
