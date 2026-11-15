// B5 逐页截图（before/after）—— 静态 dist + 与探针同一份 API 打桩
//
// 用法（必须在 frontend/ 下跑）：
//   node ../docs/evidence/2026-11-16-shell-b5/shots-b5.mjs --dist dist-b5-before --out ../docs/evidence/2026-11-16-shell-b5/shots/before --width 1600
//
// 口径：
//   · 视口固定 1600/1280 × 1000、deviceScaleFactor=1、locale zh-CN、关动画/关光标闪烁；
//   · 每页截 viewport（不用 fullPage：页面高度会变，尺寸差会把「同一位置 2px」的结论搅浑）；
//   · 另存一张 `*.empty.png`：把第一个 `.panel-empty` 的包围盒上下各外扩 24px 裁切 ——
//     本批的变化全在空态块上，逐页看这张最省眼力；
//   · 桩与 shell-probe-b5.mjs **同一份**（改一处必须同步另一处，否则探针/像素对不上）。
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';

const req = createRequire(path.join(process.cwd(), '__resolve__.js'));
const chromium = req('@playwright/test').chromium;

const argv = process.argv.slice(2);
const arg = (k, d = null) => { const i = argv.indexOf('--' + k); return i >= 0 ? argv[i + 1] : d; };
const DIST = path.resolve(arg('dist'));
const OUT = path.resolve(arg('out'));
const WIDTH = Number(arg('width', '1600'));
const PORT = Number(arg('port', '5222'));
const HERE = path.dirname(new URL(import.meta.url).pathname);
const ROUTES_FILE = arg('routes') || path.join(HERE, 'routes.txt');
const ROUTES = fs.readFileSync(ROUTES_FILE, 'utf8').split('\n').map((s) => s.trim()).filter((s) => s && !s.startsWith('#'));

const PRE_CLICKS = {
  '/business/hr/attendance': ['考勤台账'],
  '/business/hr/salary': ['我的薪资'],
  '/business/erp/ledger': ['科目余额表'],
  '/business/erp/customer-price': ['阶梯价', '价格阶梯'],
  '/business/erp/inventory': ['库存余额', '库存'],
};

const MIME = {
  '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8', '.json': 'application/json; charset=utf-8', '.svg': 'image/svg+xml',
  '.png': 'image/png', '.jpg': 'image/jpeg', '.woff': 'font/woff', '.woff2': 'font/woff2', '.ttf': 'font/ttf', '.ico': 'image/x-icon',
};
const PAGE_DATA = { list: [], total: 0, page: 1, page_size: 20 };
const ok = (d) => JSON.stringify({ code: 0, msg: 'ok', data: d });
const REPORT_STUB = {
  report_code: 'PROBE', report_name: '壳探针报表', company_name: '壳探针', unit: '元',
  as_of: '2026-11-16', as_of_source: 'TODAY', period: null, period_name: null, period_status: null,
  period_start: '2026-11-01', period_end: '2026-11-30', generated_at: '2026-11-16 00:00:00',
  buckets: [], summary: [], total_unpaid: '0', overdue_unpaid: '0', overdue_ratio: '0', total_amount: '0',
  total_paid: '0', doc_count: 0, supplier_count: 0, suppliers: [], details: [], detail_count: 0,
  detail_truncated: false, hints: [], rows: [], sections: [], items: [], lines: [], columns: [], balances: [],
  total_debit: '0', total_credit: '0', total_opening_debit: '0', total_opening_credit: '0',
  total_period_debit: '0', total_period_credit: '0', total_closing_debit: '0', total_closing_credit: '0',
  balanced: true, opening_balance: '0', closing_balance: '0', children: [],
};
function stub(reqUrl) {
  const p = new URL(reqUrl, 'http://127.0.0.1').pathname;
  if (p === '/api/system/auth/me') {
    return ok({ id: 'aaaaaaaa-0000-0000-0000-000000000001', username: 'shell_probe', real_name: '壳探针', email: 'probe@example.com', mobile: '', dept_id: null, roles: ['SUPER_ADMIN'], permissions: ['*:*:*'], must_change_password: false });
  }
  if (p === '/api/system/auth/menus') {
    return ok([{ id: 'm-dir', name: 'erp', title: '企业管理', path: '/business/erp', component: 'Layout', icon: 'app', menu_type: 1, auth_code: '', parent_id: null, sort_order: 10, is_visible: true, children: [
      { id: 'm-asset', name: 'erp-admin-asset', title: '资产管理', path: '/business/erp/asset', component: 'business/erp/asset/index', icon: 'money', menu_type: 2, auth_code: 'business:erp:asset:list', parent_id: 'm-dir', sort_order: 10, is_visible: true, children: [] },
    ] }]);
  }
  if (p === '/api/system/auth/idle-policy') return ok({ timeout_minutes: 0, warn_seconds: 0, is_super_admin: true });
  if (p === '/api/system/auth/refresh') return ok({ access_token: 'probe-token', refresh_token: 'probe-refresh', token_type: 'Bearer', expires_in: 86400 });
  if (p.startsWith('/api/support/notice') || p.startsWith('/api/system/notice')) return ok([]);
  if (p.includes('/dict/')) return ok([]);
  if (p.includes('/dept/tree')) return ok([]);
  if (p.includes('/switch')) return ok({ enabled: false, cost_enabled: false, inventory_enabled: false });
  if (/\/erp\/report\//.test(p)) return ok(REPORT_STUB);
  if (/\/erp\/ledger\/trial-balance/.test(p)) return ok(REPORT_STUB);
  if (/\/hr\/salary\/calc/.test(p)) {
    return ok({ period_code: '2026-11', period_start: '2026-11-01', period_end: '2026-11-30', attendance_period_id: null, dry_run: true, sheet: null, item_count: 8, missing_attendance: [],
      totals: { employee_count: 0, earning_total: '0', pre_deduct_total: '0', gross_total: '0', withhold_total: '0', net_total: '0', si_company_total: '0', hf_company_total: '0', accrual_total: '0' }, employees: [] });
  }
  if (/\/hr\/salary\/[^/]+$/.test(p) && !p.endsWith('/page')) {
    return ok({ id: 'bbbbbbbb-0000-0000-0000-000000000001', sheet_no: 'SAL-PROBE', period_code: '2026-11', status: 'DRAFT', status_name: '草稿',
      dept_summary: [], detail_count: 0, employee_count: 0, net_total: '0', gross_total: '0', approval_instance_id: null,
      period_start: '2026-11-01', period_end: '2026-11-30', created_at: '2026-11-16 00:00:00', can_edit: true, can_submit: true, can_post: false, can_reverse: false });
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

await new Promise((r) => server.listen(PORT, '127.0.0.1', r));
fs.mkdirSync(OUT, { recursive: true });
const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: WIDTH, height: 1000 }, deviceScaleFactor: 1, locale: 'zh-CN', timezoneId: 'Asia/Shanghai', reducedMotion: 'reduce' });
await ctx.addInitScript(() => {
  localStorage.setItem('x_access_token', 'probe-token');
  localStorage.setItem('x_refresh_token', 'probe-refresh');
  localStorage.setItem('cp-onboarding-done', '1');
  localStorage.setItem('cloudpivot.setup-wizard.skipped:shell_probe', '1');
});
const page = await ctx.newPage();
const manifest = {};
const empties = {};
for (const route of ROUTES) {
  await page.goto('http://127.0.0.1:' + PORT + route, { waitUntil: 'networkidle', timeout: 45000 }).catch(() => {});
  await page.waitForTimeout(900);
  for (const spec of PRE_CLICKS[route] || []) {
    const clicked = await page.evaluate((s) => {
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
  const box = await page.evaluate(() => {
    const el = document.querySelector('.panel-empty');
    if (!el) return null;
    const r = el.getBoundingClientRect();
    return { x: 0, y: Math.max(0, Math.round(r.y) - 24), width: window.innerWidth, height: Math.round(r.height) + 48 };
  });
  if (box) {
    const ef = path.join(OUT, slug(route) + '.empty.png');
    await page.screenshot({ path: ef, clip: { x: box.x, y: box.y, width: box.width, height: Math.min(box.height, 1000 - box.y) }, animations: 'disabled', caret: 'hide' });
    empties[route] = ef;
  }
  manifest[route] = file;
  console.log(`${route} → ${file}`);
}
fs.writeFileSync(path.join(OUT, 'manifest.json'), JSON.stringify({ width: WIDTH, shots: manifest, empties }, null, 1));
await browser.close();
server.close();
console.log('共 ' + Object.keys(manifest).length + ' 张');
