#!/usr/bin/env node
/** D3-UI 部署后冒烟：换 dist 后其它页面不白屏、无控制台 error（只读，不写数据） */
import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';
const BASE = 'http://127.0.0.1';
const PW = process.env.NIK_ADMIN_PASSWORD || 'Admin@123456';
const lr = await fetch(`${BASE}/api/system/auth/login`, {
  method: 'POST', headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ username: 'admin', password: PW }),
});
const lg = await lr.json();
const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1500, height: 950 } });
await ctx.addInitScript(([a, r]) => {
  localStorage.setItem('x_access_token', a);
  localStorage.setItem('x_refresh_token', r);
  localStorage.setItem('x_guide_done', '1');
  localStorage.setItem('cp-onboarding-done', '1');
}, [lg.data.access_token, lg.data.refresh_token]);
const page = await ctx.newPage();
let fail = 0;
for (const path of ['/dashboard', '/task/list', '/flow/def', '/business/erp/stock-out', '/business/erp/purchase-order', '/system/menu']) {
  const errs = [];
  const http = [];
  page.on('console', (m) => { if (m.type() === 'error') errs.push(m.text()); });
  page.on('pageerror', (e) => errs.push(String(e)));
  page.on('response', (r) => { if (r.status() >= 400 && r.url().includes('/api/')) http.push(`${r.status()} ${r.url()}`); });
  await page.goto(`${BASE}${path}`, { waitUntil: 'networkidle', timeout: 45000 }).catch(() => {});
  await page.waitForTimeout(1500);
  const text = ((await page.locator('body').innerText().catch(() => '')) || '').replace(/\s+/g, ' ').trim();
  const rows = await page.locator('tbody tr').count();
  const bad = text.length < 40 || errs.length > 0;
  if (bad) fail += 1;
  console.log(`${bad ? '[FAIL]' : '[PASS]'} ${path}  文本长度=${text.length} 表格行=${rows} console_err=${errs.length} http_err=${http.length}`);
  if (errs.length) console.log('   errs=', errs.slice(0, 2));
  if (http.length) console.log('   http=', http.slice(0, 2));
  page.removeAllListeners('console');
  page.removeAllListeners('pageerror');
  page.removeAllListeners('response');
}
await browser.close();
console.log(fail ? `冒烟失败 ${fail} 页` : '冒烟全部通过（5 页）');
process.exit(fail ? 1 : 0);
