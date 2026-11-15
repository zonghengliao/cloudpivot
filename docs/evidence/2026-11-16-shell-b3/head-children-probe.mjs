// 针对性探针：hr/employee 无建档权限态下，`.panel-head` 的**所有直接子元素**几何
// （`.filter-row__hint` 不是壳类，主探针不收它；这条决定 `--end` 加 margin-left:auto 会不会把它挤走）
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';

const req = createRequire(path.join(process.cwd(), '__resolve__.js'));
const chromium = req('@playwright/test').chromium;
const argv = process.argv.slice(2);
const arg = (k, d = null) => { const i = argv.indexOf('--' + k); return i >= 0 ? argv[i + 1] : d; };
const DIST = path.resolve(arg('dist'));
const OUT = arg('out');

const MIME = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8', '.json': 'application/json; charset=utf-8', '.svg': 'image/svg+xml', '.png': 'image/png', '.woff2': 'font/woff2', '.woff': 'font/woff', '.ttf': 'font/ttf', '.ico': 'image/x-icon', '.mjs': 'text/javascript; charset=utf-8' };
const ok = (d) => JSON.stringify({ code: 0, msg: 'ok', data: d });
const P = ['business:hr:employee:page', 'business:hr:employee:list', 'business:hr:employee:detail'];
function stub(u) {
  const p = new URL(u, 'http://127.0.0.1').pathname;
  if (p === '/api/system/auth/me') return ok({ id: 'a', username: 'probe', real_name: '壳探针', email: '', mobile: '', dept_id: null, roles: ['EMPLOYEE'], permissions: P, must_change_password: false });
  if (p === '/api/system/auth/menus') return ok([]);
  if (p === '/api/system/auth/idle-policy') return ok({ timeout_minutes: 0, warn_seconds: 0, is_super_admin: false });
  if (p === '/api/system/auth/refresh') return ok({ access_token: 'probe-token', refresh_token: 'probe-refresh', token_type: 'Bearer', expires_in: 86400 });
  if (p.startsWith('/api/support/notice') || p.startsWith('/api/system/notice')) return ok([]);
  if (p.includes('/dict/')) return ok([]);
  if (p.includes('/dept/tree')) return ok([]);
  return ok({ list: [], total: 0, page: 1, page_size: 20 });
}
const server = http.createServer((rq, rs) => {
  const u = new URL(rq.url, 'http://127.0.0.1');
  if (u.pathname.startsWith('/api/')) { rs.writeHead(200, { 'content-type': 'application/json; charset=utf-8', 'cache-control': 'no-store' }); rs.end(stub(rq.url)); return; }
  let f = path.join(DIST, decodeURIComponent(u.pathname));
  if (!fs.existsSync(f) || fs.statSync(f).isDirectory()) f = path.join(DIST, 'index.html');
  rs.writeHead(200, { 'content-type': MIME[path.extname(f).toLowerCase()] || 'application/octet-stream' });
  fs.createReadStream(f).pipe(rs);
});
await new Promise((r) => server.listen(5203, '127.0.0.1', r));
const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1600, height: 1000 }, locale: 'zh-CN', deviceScaleFactor: 1 });
await ctx.addInitScript(() => { localStorage.setItem('x_access_token', 'probe-token'); localStorage.setItem('x_refresh_token', 'probe-refresh'); localStorage.setItem('cp-onboarding-done', '1'); });
const page = await ctx.newPage();
await page.goto('http://127.0.0.1:5203/business/hr/employee', { waitUntil: 'networkidle', timeout: 45000 }).catch(() => {});
await page.waitForTimeout(1200);
const out = await page.evaluate(() => {
  const heads = [...document.querySelectorAll('.panel-head')];
  return heads.map((h) => ({
    head: ((r) => ({ x: r.x, y: r.y, w: r.width, h: r.height }))(h.getBoundingClientRect()),
    justify: getComputedStyle(h).justifyContent,
    children: [...h.children].map((c) => ({
      tag: c.tagName.toLowerCase(), cls: c.getAttribute('class') || '', text: (c.textContent || '').trim().slice(0, 24),
      x: Math.round(c.getBoundingClientRect().x * 10) / 10, y: Math.round(c.getBoundingClientRect().y * 10) / 10,
      w: Math.round(c.getBoundingClientRect().width * 10) / 10, h: Math.round(c.getBoundingClientRect().height * 10) / 10,
      ml: getComputedStyle(c).marginLeft,
    })),
  }));
});
console.log(JSON.stringify(out, null, 1));
if (OUT) fs.writeFileSync(OUT, JSON.stringify(out, null, 1));
await browser.close(); server.close();
