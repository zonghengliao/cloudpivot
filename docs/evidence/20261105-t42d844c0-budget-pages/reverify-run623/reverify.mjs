/**
 * t_42d844c0 二次独立复验（run 623）：只读，不建不提交任何数据。
 * 断言三条预算路由真机打开 404 计数 = 0，并留 5 张图（列表/新建弹窗/执行/映射/映射弹窗）。
 */
import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';
import fs from 'node:fs';

const BASE = process.env.BASE || 'http://127.0.0.1:5180';
const API = process.env.API || 'http://127.0.0.1:18097';
const OUT = process.env.OUT || '/root/.hermes/profiles/frontend-developer/cache/scratch/t42d-reverify';
fs.mkdirSync(OUT, { recursive: true });
for (const f of fs.readdirSync(OUT)) if (f.endsWith('.png')) fs.unlinkSync(`${OUT}/${f}`);

const loginRes = await fetch(`${API}/api/system/auth/login`, {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ username: 'admin', password: 'Manual@123456' }),
});
const loginBody = await loginRes.json();
if (!loginBody?.data?.access_token) {
  console.log('LOGIN_FAIL', loginRes.status, JSON.stringify(loginBody).slice(0, 300));
  process.exit(2);
}
const { access_token: token, refresh_token: refresh } = loginBody.data;
console.log('LOGIN ok admin/SUPER_ADMIN');

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
    errors.push(`${r.status()} ${r.url().split('?')[0]}`);
  }
});
page.on('pageerror', (e) => errors.push('pageerror: ' + String(e).split('\n')[0].slice(0, 200)));

let idx = 0;
const results = [];
async function shot(name) {
  idx += 1;
  const file = `${OUT}/${String(idx).padStart(2, '0')}-${name}.png`;
  await page.screenshot({ path: file });
  console.log('[shot]', file);
  return file;
}
async function closeUnreadDialog() {
  const btn = page.getByTestId('login-unread-ok');
  if (await btn.count()) {
    await btn.click().catch(() => {});
    await page.waitForTimeout(400);
  }
}
async function check(route, name) {
  await page.goto(`${BASE}${route}`, { waitUntil: 'domcontentloaded' });
  await page.waitForTimeout(1800);
  await closeUnreadDialog();
  const nf = await page.locator('text=您访问的页面不存在').count();
  const txt = (await page.locator('body').innerText().catch(() => '')).replace(/\s+/g, ' ');
  const file = await shot(name);
  results.push({ route, nf404: nf, file, excerpt: txt.slice(0, 160) });
  console.log('[nav]', route, 'nf404=', nf, '| excerpt:', txt.slice(0, 90));
  return nf;
}

await check('/business/erp/budget', 'budget-list');
// 列表页动作：打开「新建预算」弹窗（只看不存）
const createBtn = page.getByRole('button', { name: /新建预算/ });
if (await createBtn.count()) {
  await createBtn.first().click();
  await page.waitForTimeout(1200);
  await shot('budget-create-dialog');
  console.log('[action] 新建预算弹窗已打开');
  await page.keyboard.press('Escape').catch(() => {});
  await page.waitForTimeout(500);
} else {
  console.log('[action] WARN 未找到「新建预算」按钮');
}
await check('/business/erp/budget/report', 'budget-report');
await check('/business/erp/budget/mapping', 'budget-mapping');
const newMap = page.getByRole('button', { name: /新建映射|新增映射/ });
if (await newMap.count()) {
  await newMap.first().click();
  await page.waitForTimeout(1200);
  await shot('budget-mapping-dialog');
  console.log('[action] 新建映射弹窗已打开');
}

const total404 = results.reduce((s, r) => s + r.nf404, 0);
console.log('\n=== SUMMARY ===');
for (const r of results) console.log(`route ${r.route} 404=${r.nf404}`);
console.log('TOTAL_404 =', total404);
console.log('API/PAGE ERRORS =', errors.length ? errors.slice(0, 10).join(' | ') : 'none');
fs.writeFileSync(`${OUT}/summary.json`, JSON.stringify({ results, total404, errors }, null, 2));
await browser.close();
process.exit(total404 === 0 ? 0 : 1);
