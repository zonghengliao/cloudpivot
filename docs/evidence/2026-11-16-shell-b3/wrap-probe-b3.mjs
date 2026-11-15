// B3 特有验收：工具行**换行点**探针
//
// 宽度归一后最容易出的副作用是「某一页的筛选项从 1 排变 2 排（或反之）」。这里把每个
// `.tool-row` / ListPage 的 `.lp__filter-row` 的**行数**、每行包含哪些控件（按可见文本/类名）
// 直接算出来，before/after 逐页比对 —— 比肉眼看截图准，也便于写进证据表。
//
// 用法（在 frontend/ 下跑）：
//   node wrap-probe-b3.mjs --dist <dist> --out <json> [--routes routes.txt]
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
const HERE = path.dirname(new URL(import.meta.url).pathname);
const ROUTES_FILE = arg('routes');
const ROUTES = (ROUTES_FILE ? fs.readFileSync(ROUTES_FILE, 'utf8') : fs.readFileSync(path.join(HERE, 'routes.txt'), 'utf8'))
  .split('\n').map((s) => s.trim()).filter((s) => s && !s.startsWith('#'));

const PRE_CLICKS = {
  '/business/erp/ledger?probe=trial': ['科目余额表'],
  '/business/erp/cost-period': ['testid:cp-btn-new', 'testid:cp-btn-next'],
  '/business/hr/salary': ['testid:hr-salary-preview', 'testid:hr-salary-calc-run'],
};
const MIME = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.css': 'text/css; charset=utf-8', '.json': 'application/json; charset=utf-8', '.svg': 'image/svg+xml', '.png': 'image/png', '.woff2': 'font/woff2', '.woff': 'font/woff', '.ttf': 'font/ttf', '.ico': 'image/x-icon', '.mjs': 'text/javascript; charset=utf-8' };
const ok = (d) => JSON.stringify({ code: 0, msg: 'ok', data: d });
const PERMS_ALL = ['*:*:*'];
function stub(u) {
  const p = new URL(u, 'http://127.0.0.1').pathname;
  if (p === '/api/system/auth/me') return ok({ id: 'a', username: 'shell_probe', real_name: '壳探针', email: '', mobile: '', dept_id: null, roles: ['SUPER_ADMIN'], permissions: PERMS_ALL, must_change_password: false });
  if (p === '/api/system/auth/menus') return ok([{ id: 'd', name: 'erp', title: '企业管理', path: '/business/erp', component: 'Layout', icon: 'app', menu_type: 1, auth_code: '', parent_id: null, sort_order: 10, is_visible: true, children: [] }]);
  if (p === '/api/system/auth/idle-policy') return ok({ timeout_minutes: 0, warn_seconds: 0, is_super_admin: true });
  if (p === '/api/system/auth/refresh') return ok({ access_token: 'probe-token', refresh_token: 'probe-refresh', token_type: 'Bearer', expires_in: 86400 });
  if (p.startsWith('/api/support/notice') || p.startsWith('/api/system/notice')) return ok([]);
  if (p.includes('/dict/')) return ok([]);
  if (p.includes('/dept/tree')) return ok([]);
  if (p.includes('/switch')) return ok({ enabled: false, cost_enabled: false, inventory_enabled: false });
  if (p.includes('/erp/ledger/trial-balance')) return ok({ period: '2026-11', period_start: '2026-11-01', period_end: '2026-11-30', rows: [], total_opening_debit: '0', total_opening_credit: '0', total_period_debit: '0', total_period_credit: '0', total_closing_debit: '0', total_closing_credit: '0', balanced: true });
  if (p.includes('/erp/report/payable-aging')) return ok({ report_code: 'PAYABLE_AGING', report_name: '应付账龄分析表', company_name: '壳探针', unit: '元', as_of: '2026-11-16', as_of_source: 'TODAY', period: null, period_name: null, period_status: null, generated_at: '2026-11-16 00:00:00', buckets: [], summary: [], total_unpaid: '0', overdue_unpaid: '0', overdue_ratio: '0', total_amount: '0', total_paid: '0', doc_count: 0, supplier_count: 0, suppliers: [], details: [], detail_count: 0, detail_truncated: false, hints: [] });
  if (p.includes('/hr/salary/calc')) {
    const emp = (no, name, dept) => ({ employee_id: 'emp-' + no, employee_no: no, employee_name: name, dept_name: dept, should_days: '21.75', actual_days: '21.75', leave_unpaid_days: '0', ot_hours: '0', earning_amount: '12000.00', pre_deduct_amount: '0.00', gross_amount: '12000.00', withhold_amount: '1500.00', net_amount: '10500.00' });
    return ok({ period_code: '2026-11', period_start: '2026-11-01', period_end: '2026-11-30', attendance_period_id: null, dry_run: true, sheet: null, totals: { employee_count: 2, earning_total: '24000.00', pre_deduct_total: '0.00', gross_total: '24000.00', withhold_total: '3000.00', net_total: '21000.00', si_company_total: '3000.00', hf_company_total: '1500.00', accrual_total: '28500.00' }, item_count: 8, missing_attendance: [], employees: [emp('E0001', '张三', '财务部'), emp('E0002', '李四', '生产部')] });
  }
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

const SEL = '.tool-row, .lp__filter-row';
const collect = (page) => page.evaluate((sel) => {
  const label = (el) => {
    const t = (el.getAttribute('placeholder') || el.textContent || '').trim().replace(/\s+/g, ' ').slice(0, 18);
    const c = (el.getAttribute('class') || '').split(/\s+/).find((x) => /^tool-row__|^lp__/.test(x)) || '';
    return (c ? c + ':' : '') + (t || el.tagName.toLowerCase());
  };
  return [...document.querySelectorAll(sel)].map((row, idx) => {
    const rr = row.getBoundingClientRect();
    // 0×0 的子项（`<span class="lp__spacer"/>` / `<span class="tool-row__spacer"/>` 这类自闭合空元素）
    // 没有视觉体积、top 又是行首 y，会把「1 排」误算成 2 排 —— 直接排除
    const kids = [...row.children]
      .map((c) => ({ label: label(c), top: Math.round(c.getBoundingClientRect().top), x: Math.round(c.getBoundingClientRect().left), w: Math.round(c.getBoundingClientRect().width), h: Math.round(c.getBoundingClientRect().height) }))
      .filter((k) => k.w > 1 && k.h > 1);
    const tops = [...new Set(kids.map((k) => k.top))].sort((a, b) => a - b);
    // 把 top 归并成「行」：同排容差 10px
    const rows = [];
    for (const k of kids) {
      const line = rows.find((r) => Math.abs(r.top - k.top) <= 10);
      if (line) line.items.push(k.label);
      else rows.push({ top: k.top, items: [k.label] });
    }
    return { idx, cls: row.getAttribute('class'), x: Math.round(rr.left), y: Math.round(rr.top), w: Math.round(rr.width), h: Math.round(rr.height), lineCount: rows.length, lines: rows.sort((a, b) => a.top - b.top).map((r) => r.items) };
  });
}, SEL);

await new Promise((r) => server.listen(5204, '127.0.0.1', r));
const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1600, height: 1000 }, locale: 'zh-CN', deviceScaleFactor: 1 });
await ctx.addInitScript(() => {
  localStorage.setItem('x_access_token', 'probe-token');
  localStorage.setItem('x_refresh_token', 'probe-refresh');
  localStorage.setItem('cp-onboarding-done', '1');
  localStorage.setItem('cloudpivot.setup-wizard.skipped:shell_probe', '1');
});
const page = await ctx.newPage();
const out = { dist: DIST, routes: {} };
for (const route of ROUTES) {
  await page.goto('http://127.0.0.1:5204' + route, { waitUntil: 'networkidle', timeout: 45000 }).catch(() => {});
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
        if ((it.textContent || '').trim() === s) { (it.querySelector('.t-tabs__nav-item-text-wrapper') || it).dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true })); return true; }
      }
      return false;
    }, spec);
    if (clicked) await page.waitForTimeout(1200);
  }
  out.routes[route] = await collect(page);
  const rows = out.routes[route].map((r) => r.lineCount).join(',') || '-';
  console.log(`${route}  工具行 ${out.routes[route].length} 个 / 各自行数 [${rows}]`);
}
fs.writeFileSync(OUT, JSON.stringify(out, null, 1));
console.log('→ ' + OUT);
await browser.close();
server.close();
