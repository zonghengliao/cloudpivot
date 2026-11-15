// 调试：单路由渲染诊断（B2）
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';
const req = createRequire(path.join(process.cwd(), '__resolve__.js'));
const chromium = req('@playwright/test').chromium;
const DIST = path.resolve(process.argv[2]);
const ROUTE = process.argv[3];
const MIME = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.png': 'image/png', '.woff2': 'font/woff2', '.json': 'application/json' };
const PAGE_DATA = { list: [], total: 0, page: 1, page_size: 20 };
const ok = (d) => JSON.stringify({ code: 0, msg: 'ok', data: d });
function stub(url) {
  const p = new URL(url, 'http://x').pathname;
  if (p === '/api/system/auth/me') return ok({ id: 'a', username: 'p', real_name: 'p', roles: ['SUPER_ADMIN'], permissions: ['*:*:*'], must_change_password: false, dept_id: null, email: '', mobile: '' });
  if (p === '/api/system/auth/menus') return ok([]);
  if (p === '/api/system/auth/idle-policy') return ok({ timeout_minutes: 0, warn_seconds: 0, is_super_admin: true });
  if (p === '/api/system/auth/refresh') return ok({ access_token: 't', refresh_token: 'r', token_type: 'Bearer', expires_in: 86400 });
  if (p.startsWith('/api/support/notice') || p.startsWith('/api/system/notice')) return ok([]);
  if (p.includes('/dict/')) return ok([]);
  if (p.includes('/dept/tree')) return ok([]);
  if (p.includes('/switch')) return ok({ enabled: false, cost_enabled: false, inventory_enabled: false });
  return ok(PAGE_DATA);
}
const server = http.createServer((rq, rs) => {
  const u = new URL(rq.url, 'http://127.0.0.1');
  if (u.pathname.startsWith('/api/')) { rs.writeHead(200, { 'content-type': 'application/json' }); rs.end(stub(rq.url)); return; }
  let f = path.join(DIST, decodeURIComponent(u.pathname));
  if (!fs.existsSync(f) || fs.statSync(f).isDirectory()) f = path.join(DIST, 'index.html');
  rs.writeHead(200, { 'content-type': MIME[path.extname(f)] || 'application/octet-stream' });
  fs.createReadStream(f).pipe(rs);
});
await new Promise((r) => server.listen(5201, '127.0.0.1', r));
const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1600, height: 1000 }, locale: 'zh-CN' });
await ctx.addInitScript(() => { localStorage.setItem('x_access_token', 'probe-token'); localStorage.setItem('x_refresh_token', 'probe-refresh'); localStorage.setItem('cp-onboarding-done', '1'); });
const page = await ctx.newPage();
const errs = [];
page.on('pageerror', (e) => errs.push('pageerror: ' + String(e).slice(0, 300)));
page.on('console', (m) => { if (m.type() === 'error') errs.push('console: ' + m.text().slice(0, 200)); });
await page.goto('http://127.0.0.1:5201' + ROUTE, { waitUntil: 'networkidle', timeout: 45000 }).catch((e) => errs.push('goto: ' + e.message));
await page.waitForTimeout(1500);
const TAB = process.argv[4];
if (TAB) {
  const clicked = await page.evaluate((text) => {
    for (const it of document.querySelectorAll('.t-tabs__nav-item')) {
      if ((it.textContent || '').trim() === text) {
        (it.querySelector('.t-tabs__nav-item-text-wrapper') || it).dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
        return true;
      }
    }
    return false;
  }, TAB);
  console.log('点击页签', TAB, '→', clicked);
  await page.waitForTimeout(1500);
}
console.log('URL:', page.url());
console.log('错误:', errs.length ? errs.join('\n  ') : '(无)');
console.log('tabs:', await page.evaluate(() => [...document.querySelectorAll('.t-tabs__nav-item')].map((n) => n.textContent.trim())));
console.log('shell 元素数:', await page.evaluate(() => document.querySelectorAll('[class*="panel-head"],[class*="tool-row"],[class*="tool-card"],[class*="list-card"]').length));
console.log('body 文本前 300:', (await page.evaluate(() => document.body.innerText)).slice(0, 300).replace(/\n/g, ' | '));
await browser.close();
server.close();
