// B2 逐页截图（before/after）—— 静态 dist + 与探针同一份 API 打桩
//
// 用法（必须在 frontend/ 下跑）：
//   node ../docs/evidence/2026-11-16-shell-b2/shots.mjs --dist /tmp/b2/dist-before --out /tmp/b2/shots/before
//   node ../docs/evidence/2026-11-16-shell-b2/shots.mjs --dist /tmp/b2/dist-after  --out /tmp/b2/shots/after
//
// 口径：
//   · 视口固定 1600×1000、deviceScaleFactor=1、locale zh-CN、关动画/关光标闪烁（否则像素 diff 全是噪声）；
//   · 与 shell-probe-b2.mjs 同一套路由与预点击（Tab / 向导步都要先点开，否则截的是别的页）；
//   · 每页截 viewport（不是 fullPage）：页面高度会随内容变，fullPage 的尺寸差异会掩盖「同一位置 2px」的结论。
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';

const req = createRequire(path.join(process.cwd(), '__resolve__.js'));
const chromium = req('@playwright/test').chromium;

const argv = process.argv.slice(2);
const arg = (k, d = null) => {
  const i = argv.indexOf('--' + k);
  return i >= 0 ? argv[i + 1] : d;
};
const DIST = path.resolve(arg('dist'));
const OUT = path.resolve(arg('out'));

const ROUTES = [
  '/business/erp/asset-category',
  '/business/erp/asset',
  '/business/erp/budget',
  '/business/erp/budget/mapping',
  '/business/erp/budget/report',
  '/business/erp/cost-period',
  '/business/erp/depreciation',
  '/business/erp/expense-type',
  '/business/erp/expense',
  '/business/erp/expense/pay',
  '/business/erp/invoice',
  '/business/erp/ledger?probe=trial',
  '/business/erp/price',
  '/business/erp/receipt-refund',
  '/business/erp/receipt',
  '/business/erp/receivable',
  '/business/erp/receivable?probe=aging',
  '/business/erp/report/balance-sheet',
  '/business/erp/report/cash-flow',
  '/business/erp/report/income-statement',
  '/business/erp/report/payable-aging',
  '/business/erp/report/report-item',
  '/business/erp/sales-order',
  '/business/erp/sales-return',
  '/business/erp/shipment',
];

const PRE_CLICKS = {
  '/business/erp/receivable?probe=aging': ['账龄看板'],
  '/business/erp/ledger?probe=trial': ['科目余额表'],
  '/business/erp/cost-period': ['testid:cp-btn-new', 'testid:cp-btn-next'],
};

const MIME = {
  '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8', '.json': 'application/json; charset=utf-8', '.svg': 'image/svg+xml',
  '.png': 'image/png', '.jpg': 'image/jpeg', '.woff': 'font/woff', '.woff2': 'font/woff2', '.ttf': 'font/ttf', '.ico': 'image/x-icon',
};
const PAGE_DATA = { list: [], total: 0, page: 1, page_size: 20 };
const ok = (d) => JSON.stringify({ code: 0, msg: 'ok', data: d });
function stub(reqUrl) {
  const p = new URL(reqUrl, 'http://127.0.0.1').pathname;
  if (p === '/api/system/auth/me') {
    return ok({ id: 'aaaaaaaa-0000-0000-0000-000000000001', username: 'shell_probe', real_name: '壳探针', email: 'probe@example.com', mobile: '', dept_id: null, roles: ['SUPER_ADMIN'], permissions: ['*:*:*'], must_change_password: false });
  }
  if (p === '/api/system/auth/menus') {
    return ok([{ id: 'm-dir', name: 'erp', title: '企业管理', path: '/business/erp', component: 'Layout', icon: 'app', menu_type: 1, auth_code: '', parent_id: null, sort_order: 10, is_visible: true, children: [
      { id: 'm-asset', name: 'erp-admin-asset', title: '资产管理', path: '/business/erp/asset', component: 'business/erp/asset/index', icon: 'money', menu_type: 2, auth_code: 'business:erp:asset:list', parent_id: 'm-dir', sort_order: 10, is_visible: true, children: [] },
      { id: 'm-contract', name: 'erp-contract', title: '合同管理', path: '/business/erp/contract', component: 'business/erp/contract/index', icon: 'file', menu_type: 2, auth_code: 'business:erp:contract:list', parent_id: 'm-dir', sort_order: 20, is_visible: true, children: [] },
    ] }]);
  }
  if (p === '/api/system/auth/idle-policy') return ok({ timeout_minutes: 0, warn_seconds: 0, is_super_admin: true });
  if (p === '/api/system/auth/refresh') return ok({ access_token: 'probe-token', refresh_token: 'probe-refresh', token_type: 'Bearer', expires_in: 86400 });
  if (p.startsWith('/api/support/notice') || p.startsWith('/api/system/notice')) return ok([]);
  if (p.includes('/dict/')) return ok([]);
  if (p.includes('/dept/tree')) return ok([]);
  if (p.includes('/switch')) return ok({ enabled: false, cost_enabled: false, inventory_enabled: false });
  if (p.includes('/erp/ledger/trial-balance')) {
    return ok({ period: '2026-11', period_start: '2026-11-01', period_end: '2026-11-30', rows: [], total_opening_debit: '0', total_opening_credit: '0', total_period_debit: '0', total_period_credit: '0', total_closing_debit: '0', total_closing_credit: '0', balanced: true });
  }
  if (p.includes('/erp/report/payable-aging')) {
    return ok({ report_code: 'PAYABLE_AGING', report_name: '应付账龄分析表', company_name: '壳探针', unit: '元', as_of: '2026-11-16', as_of_source: 'TODAY', period: null, period_name: null, period_status: null, generated_at: '2026-11-16 00:00:00', buckets: [], summary: [], total_unpaid: '0', overdue_unpaid: '0', overdue_ratio: '0', total_amount: '0', total_paid: '0', doc_count: 0, supplier_count: 0, suppliers: [], details: [], detail_count: 0, detail_truncated: false, hints: [] });
  }
  return ok(PAGE_DATA);
}
const server = http.createServer((rq, rs) => {
  const u = new URL(rq.url, 'http://127.0.0.1');
  if (u.pathname.startsWith('/api/') || u.pathname === '/health') {
    rs.writeHead(200, { 'content-type': 'application/json; charset=utf-8', 'cache-control': 'no-store' });
    rs.end(stub(rq.url));
    return;
  }
  let file = path.join(DIST, decodeURIComponent(u.pathname));
  if (!fs.existsSync(file) || fs.statSync(file).isDirectory()) file = path.join(DIST, 'index.html');
  rs.writeHead(200, { 'content-type': MIME[path.extname(file).toLowerCase()] || 'application/octet-stream', 'cache-control': 'no-store' });
  fs.createReadStream(file).pipe(rs);
});

const slug = (r) => r.replace(/^\//, '').replace(/[/?=&]/g, '_').replace(/_+$/, '');

await new Promise((r) => server.listen(5202, '127.0.0.1', r));
fs.mkdirSync(OUT, { recursive: true });
const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1600, height: 1000 }, deviceScaleFactor: 1, locale: 'zh-CN', timezoneId: 'Asia/Shanghai', reducedMotion: 'reduce' });
await ctx.addInitScript(() => {
  localStorage.setItem('x_access_token', 'probe-token');
  localStorage.setItem('x_refresh_token', 'probe-refresh');
  localStorage.setItem('cp-onboarding-done', '1');
});
const page = await ctx.newPage();
const manifest = {};
for (const route of ROUTES) {
  await page.goto('http://127.0.0.1:5202' + route, { waitUntil: 'networkidle', timeout: 45000 }).catch(() => {});
  await page.waitForTimeout(900);
  for (const spec of PRE_CLICKS[route] || []) {
    const clicked = await page.evaluate((s) => {
      if (s.startsWith('testid:')) {
        const el = document.querySelector(`[data-testid="${s.slice(7)}"]`);
        if (!el) return false;
        el.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
        return true;
      }
      for (const it of document.querySelectorAll('.t-tabs__nav-item')) {
        if ((it.textContent || '').trim() === s) {
          (it.querySelector('.t-tabs__nav-item-text-wrapper') || it).dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
          return true;
        }
      }
      return false;
    }, spec);
    if (clicked) await page.waitForTimeout(1200);
  }
  await page.addStyleTag({ content: '*,*::before,*::after{transition:none!important;animation:none!important;caret-color:transparent!important}' });
  await page.waitForTimeout(250);
  const file = path.join(OUT, slug(route) + '.png');
  await page.screenshot({ path: file, animations: 'disabled', caret: 'hide' });
  manifest[route] = file;
  console.log(`${route} → ${file}`);
}
fs.writeFileSync(path.join(OUT, 'manifest.json'), JSON.stringify(manifest, null, 1));
await browser.close();
server.close();
console.log('共 ' + Object.keys(manifest).length + ' 张');
