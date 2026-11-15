// 排障小脚本：确认静态 dist 在无头 Chromium 里到底渲染成什么（B4 用）
// 用法：cd frontend && node ../docs/evidence/2026-11-16-shell-b4/debug-b4.mjs --dist <dist> [--width 1600]
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';

const req = createRequire(path.join(process.cwd(), '__resolve__.js'));
const chromium = req('@playwright/test').chromium;
const argv = process.argv.slice(2);
const arg = (k, d = null) => { const i = argv.indexOf('--' + k); return i >= 0 ? argv[i + 1] : d; };
const DIST = path.resolve(arg('dist'));
const WIDTH = Number(arg('width', '1600'));
const ROUTE = arg('route', '/business/erp/account');

const MIME = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8', '.json': 'application/json; charset=utf-8', '.svg': 'image/svg+xml', '.png': 'image/png', '.woff2': 'font/woff2', '.ico': 'image/x-icon' };
const server = http.createServer((rq, rs) => {
  const u = new URL(rq.url, 'http://127.0.0.1');
  if (u.pathname.startsWith('/api/') || u.pathname === '/health') {
    rs.writeHead(200, { 'content-type': 'application/json; charset=utf-8' });
    rs.end(JSON.stringify({ code: 0, msg: 'ok', data: {} }));
    return;
  }
  let file = path.join(DIST, decodeURIComponent(u.pathname));
  if (!fs.existsSync(file) || fs.statSync(file).isDirectory()) file = path.join(DIST, 'index.html');
  rs.writeHead(200, { 'content-type': MIME[path.extname(file).toLowerCase()] || 'application/octet-stream', 'cache-control': 'no-store' });
  fs.createReadStream(file).pipe(rs);
});
await new Promise((r) => server.listen(5239, '127.0.0.1', r));
const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: WIDTH, height: 1000 }, locale: 'zh-CN' });
await ctx.addInitScript(() => {
  try {
    localStorage.setItem('x_access_token', 'probe-token');
    localStorage.setItem('x_refresh_token', 'probe-refresh');
    localStorage.setItem('cp-onboarding-done', '1');
    localStorage.setItem('cloudpivot.setup-wizard.skipped:shell_probe', '1');
  } catch (e) { /* about:blank 里会抛 SecurityError，忽略 */ }
});
const page = await ctx.newPage();
page.on('pageerror', (e) => console.log('PAGEERROR:', String(e).slice(0, 200)));
page.on('console', (m) => { if (m.type() === 'error') console.log('CONSOLE-ERR:', m.text().slice(0, 200)); });
await page.goto('http://127.0.0.1:5239' + ROUTE, { waitUntil: 'networkidle', timeout: 45000 }).catch((e) => console.log('GOTO-ERR', e.message));
await page.waitForTimeout(1500);
console.log('url =', page.url());
console.log('title =', await page.title());
const SEL = arg('sel', null);
if (SEL) {
  const info = await page.evaluate((sel) => Array.from(document.querySelectorAll(sel)).map((el) => {
    const cs = getComputedStyle(el);
    return {
      attrs: Array.from(el.attributes).map((a) => a.name + '=' + a.value.slice(0, 60)),
      display: cs.getPropertyValue('display'),
      alignItems: cs.getPropertyValue('align-items'),
      gap: cs.getPropertyValue('gap'),
      rect: { x: Math.round(el.getBoundingClientRect().x), w: Math.round(el.getBoundingClientRect().width) },
    };
  }), SEL);
  console.log('SEL', SEL, JSON.stringify(info, null, 1));
}
console.log('ls =', await page.evaluate(() => { try { return localStorage.getItem('x_access_token'); } catch (e) { return 'ERR ' + e.message; } }));
console.log('body.len =', (await page.evaluate(() => document.body.innerHTML.length)));
console.log('body.head =', (await page.evaluate(() => document.body.innerHTML.slice(0, 400))));
console.log('shell.count =', await page.evaluate(() => document.querySelectorAll('[class*="panel-head"], [class*="tool-card"], [class*="list-card"]').length));
await browser.close();
server.close();
