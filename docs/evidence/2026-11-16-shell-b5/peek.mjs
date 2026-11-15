// 单页快照 + DOM 概况（排障用：为什么某页的空态没渲染出来）
// 用法： node peek.mjs --dist dist-b5-after --route "/business/flow/def?view=mine" --out /tmp/peek.png
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';

const req = createRequire(path.join(process.cwd(), '__resolve__.js'));
const chromium = req('@playwright/test').chromium;
const arg = (k, d = null) => { const i = process.argv.indexOf('--' + k); return i >= 0 ? process.argv[i + 1] : d; };
const DIST = path.resolve(arg('dist'));
const ROUTE = arg('route');
const OUT = path.resolve(arg('out', '/tmp/peek.png'));
const WIDTH = Number(arg('width', '1600'));
const PORT = Number(arg('port', '5244'));

const MIME = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.mjs': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8', '.json': 'application/json', '.svg': 'image/svg+xml', '.png': 'image/png', '.woff2': 'font/woff2', '.ico': 'image/x-icon' };
const PAGE_DATA = { list: [], total: 0, page: 1, page_size: 20 };
const ok = (d) => JSON.stringify({ code: 0, msg: 'ok', data: d });
function stub(reqUrl) {
  const p = new URL(reqUrl, 'http://x').pathname;
  if (p === '/api/system/auth/me') return ok({ id: 'a', username: 'probe', real_name: '壳探针', email: '', mobile: '', dept_id: null, roles: ['SUPER_ADMIN'], permissions: ['*:*:*'], must_change_password: false });
  if (p === '/api/system/auth/menus') return ok([]);
  if (p === '/api/system/auth/idle-policy') return ok({ timeout_minutes: 0, warn_seconds: 0, is_super_admin: true });
  if (p === '/api/system/auth/refresh') return ok({ access_token: 't', refresh_token: 'r', token_type: 'Bearer', expires_in: 86400 });
  if (p.includes('/dict/')) return ok([]);
  if (p.includes('/dept/tree')) return ok([]);
  return ok(PAGE_DATA);
}
const server = http.createServer((rq, rs) => {
  const u = new URL(rq.url, 'http://x');
  if (u.pathname.startsWith('/api/')) { rs.writeHead(200, { 'content-type': 'application/json' }); rs.end(stub(rq.url)); return; }
  let f = path.join(DIST, decodeURIComponent(u.pathname));
  if (!fs.existsSync(f) || fs.statSync(f).isDirectory()) f = path.join(DIST, 'index.html');
  rs.writeHead(200, { 'content-type': MIME[path.extname(f).toLowerCase()] || 'application/octet-stream' });
  fs.createReadStream(f).pipe(rs);
});
await new Promise((r) => server.listen(PORT, '127.0.0.1', r));
const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: WIDTH, height: 1000 }, locale: 'zh-CN', timezoneId: 'Asia/Shanghai' });
await ctx.addInitScript(() => {
  localStorage.setItem('x_access_token', 't'); localStorage.setItem('x_refresh_token', 'r');
  localStorage.setItem('cp-onboarding-done', '1');
  localStorage.setItem('cloudpivot.setup-wizard.skipped:shell_probe', '1');
});
const page = await ctx.newPage();
const errs = [];
page.on('pageerror', (e) => errs.push(String(e).slice(0, 200)));
page.on('console', (m) => { if (m.type() === 'error') errs.push('console: ' + m.text().slice(0, 200)); });
await page.goto('http://127.0.0.1:' + PORT + ROUTE, { waitUntil: 'networkidle', timeout: 45000 }).catch(() => {});
await page.waitForTimeout(1500);
await page.screenshot({ path: OUT });
const info = await page.evaluate(() => ({
  title: document.title,
  tabs: Array.from(document.querySelectorAll('.t-tabs__nav-item')).map((e) => (e.textContent || '').trim()),
  hasListPage: !!document.querySelector('.lp-page, .list-card, .tool-card'),
  bodyText: (document.body.innerText || '').slice(0, 400),
  panelEmpty: document.querySelectorAll('[class*="panel-empty"]').length,
}));
console.log(JSON.stringify(info, null, 1));
console.log('errors:', errs.slice(0, 5));
console.log('shot:', OUT);
await browser.close();
server.close();
