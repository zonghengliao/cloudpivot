// B6 逐页截图（before/after）—— 静态 dist + 与探针同一份 API 打桩
//
// 用法（必须在 frontend/ 下跑）：
//   node ../docs/evidence/2026-11-16-shell-b6/shots-b6.mjs --dist /tmp/b6-dist-before \
//        --out ../docs/evidence/2026-11-16-shell-b6/shots/before-1600 --width 1600
//
// 口径（照 B5 的 shots-b5.mjs）：
//   · 视口固定 1600/1280 × 1000、deviceScaleFactor=1、locale zh-CN、关动画/关光标闪烁；
//   · 每页截 viewport（不用 fullPage：页面高度会变，尺寸差会把「同一位置 2px」的结论搅浑）；
//   · 另存「带子」裁切：`*.toolband.png`（工具行所在带）/ `*.headband.png`（卡片头所在带）/
//     `*.pager.png`（分页条）—— 本批改动全在这三条带上，逐页看这张最省眼力；
//   · 桩与 shell-probe-b6.mjs **同一份**（改一处必须同步另一处，否则探针/像素对不上）。
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
const PORT = Number(arg('port', '5224'));
const HERE = path.dirname(new URL(import.meta.url).pathname);
const ROUTES_FILE = arg('routes') || path.join(HERE, 'routes.txt');
const ROUTES = fs.readFileSync(ROUTES_FILE, 'utf8').split('\n').map((s) => s.trim()).filter((s) => s && !s.startsWith('#'));

const PRE_CLICKS = {
  '/business/erp/cost-period': ['testid:cp-btn-new', 'testid:cp-btn-next'],
  '/business/erp/ledger': ['pick:testid:ledger-filter-account=1001'],
  '/business/erp/serial': ['fill:testid:serial-scan-input=SN-PROBE-1', 'click:testid:btn-scan'],
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
  slots: [], months: [],
  total_debit: '0', total_credit: '0', total_opening_debit: '0', total_opening_credit: '0',
  total_period_debit: '0', total_period_credit: '0', total_closing_debit: '0', total_closing_credit: '0',
  balanced: true, opening_balance: '0', closing_balance: '0', children: [],
};
const HELP_DOCS = {
  list: Array.from({ length: 12 }, (_, i) => ({
    id: 'cccccccc-0000-0000-0000-' + String(i + 1).padStart(12, '0'),
    title: '探针文档 ' + (i + 1), summary: '用于壳探针的分页样本',
    category_id: 'cat-1', category_name: '快速上手', status: 'PUBLISHED',
    updated_at: '2026-11-16 00:00:00', read_count: 0, version: 1,
  })),
  total: 30, page: 1, page_size: 12,
};
const HELP_TREE = [{ id: 'cat-1', name: '快速上手', parent_id: null, sort_order: 10, doc_count: 30, children: [] }];
const TRANSFER_OVERDUE = { threshold_days: 7, total: 3, oldest_days: 12 };
const SERIAL_SCAN = {
  id: 'dddddddd-0000-0000-0000-000000000001', serial_no: 'SN-PROBE-1', status: 'IN_STOCK',
  material_id: 'eeeeeeee-0000-0000-0000-000000000001', material_code: 'M-001', material_name: '探针物料',
  warehouse_id: null, warehouse_name: '', location_id: null, location_name: '',
  inbound_at: '2026-11-01 00:00:00', outbound_at: null, doc_no: '',
};
const ACCOUNT_PAGE = {
  list: [{ id: 'ffffffff-0000-0000-0000-000000000001', code: '1001', name: '库存现金', category: 'ASSET', direction: 'DEBIT', is_leaf: true, status: 1, parent_id: null }],
  total: 1, page: 1, page_size: 20,
};

function stub(reqUrl) {
  const p = new URL(reqUrl, 'http://127.0.0.1').pathname;
  if (p === '/api/system/auth/me') {
    return ok({ id: 'aaaaaaaa-0000-0000-0000-000000000001', username: 'shell_probe', real_name: '壳探针', email: 'probe@example.com', mobile: '', dept_id: null, roles: ['SUPER_ADMIN'], permissions: ['*:*:*'], must_change_password: false });
  }
  if (p === '/api/system/auth/menus') {
    return ok([{ id: 'm-dir', name: 'erp', title: '企业管理', path: '/business/erp', component: 'Layout', icon: 'app', menu_type: 1, auth_code: '', parent_id: null, sort_order: 10, is_visible: true, children: [
      { id: 'm-acc', name: 'erp-account', title: '科目', path: '/business/erp/account', component: 'business/erp/account/index', icon: 'money', menu_type: 2, auth_code: 'business:erp:account:list', parent_id: 'm-dir', sort_order: 10, is_visible: true, children: [] },
      { id: 'm-help', name: 'help-center', title: '帮助中心', path: '/help', component: 'business/help/center/index', icon: 'help', menu_type: 2, auth_code: 'system:help:read', parent_id: 'm-dir', sort_order: 20, is_visible: true, children: [] },
    ] }]);
  }
  if (p === '/api/system/auth/idle-policy') return ok({ timeout_minutes: 0, warn_seconds: 0, is_super_admin: true });
  if (p === '/api/system/auth/refresh') return ok({ access_token: 'probe-token', refresh_token: 'probe-refresh', token_type: 'Bearer', expires_in: 86400 });
  if (p.startsWith('/api/support/notice') || p.startsWith('/api/system/notice')) return ok([]);
  if (p === '/api/business/erp/transfer/overdue/summary') return ok(TRANSFER_OVERDUE);
  if (p === '/api/business/erp/serial/scan') return ok(SERIAL_SCAN);
  if (p === '/api/business/erp/account/page') return ok(ACCOUNT_PAGE);
  if (p === '/api/business/help/doc/page') return ok(HELP_DOCS);
  if (p === '/api/business/help/category/tree' || p === '/api/business/help/category/list') return ok(HELP_TREE);
  if (/^\/api\/business\/help\/doc\/[^/]+$/.test(p)) return ok({ id: 'cccccccc-0000-0000-0000-000000000001', title: '探针文档 1', content_html: '<p>探针正文</p>', toc: [], category_name: '快速上手', updated_at: '2026-11-16 00:00:00', version: 1 });
  if (p.includes('/dict/')) return ok([]);
  if (p.includes('/dept/tree')) return ok([]);
  if (p.includes('/switch')) return ok({ enabled: false, cost_enabled: false, inventory_enabled: false });
  if (/\/erp\/report\//.test(p)) return ok(REPORT_STUB);
  if (/\/erp\/budget\//.test(p)) return ok(REPORT_STUB);
  if (/\/erp\/ledger\/trial-balance/.test(p)) return ok(REPORT_STUB);
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

const tid = (id) => '[data-testid="' + id + '"]';

// 与 shell-probe-b6.mjs 同一套动作（两个脚本必须同步，否则探针/像素对不上）
async function doAction(page, spec) {
  if (spec.startsWith('fill:testid:')) {
    const head = spec.slice('fill:testid:'.length);
    const eq = head.indexOf('=');
    const id = eq >= 0 ? head.slice(0, eq) : head;
    const val = eq >= 0 ? head.slice(eq + 1) : '';
    const inner = page.locator(tid(id) + ' input, ' + tid(id) + ' textarea').first();
    if (await inner.count()) await inner.fill(val).catch(() => {});
    else await page.locator(tid(id)).first().fill(val).catch(() => {});
    return true;
  }
  if (spec.startsWith('pick:testid:')) {
    const body = spec.slice('pick:testid:'.length);
    const eq = body.indexOf('=');
    const id = eq >= 0 ? body.slice(0, eq) : body;
    const want = eq >= 0 ? body.slice(eq + 1) : '';
    const inner = page.locator(tid(id) + ' input').first();
    if (await inner.count()) await inner.click({ force: true }).catch(() => {});
    else await page.locator(tid(id)).first().click({ force: true }).catch(() => {});
    await page.waitForTimeout(300);
    await page.keyboard.press('ArrowDown').catch(() => {});
    await page.waitForTimeout(600);
    const opt = page.locator('.t-select-option', { hasText: want }).first();
    if (await opt.count()) { await opt.click({ force: true }).catch(() => {}); return true; }
    const first = page.locator('.t-select-option').first();
    if (await first.count()) { await first.click({ force: true }).catch(() => {}); return true; }
    return false;
  }
  if (spec.startsWith('scan:testid:')) {
    const body = spec.slice('scan:testid:'.length);
    const eq = body.indexOf('=');
    const id = eq >= 0 ? body.slice(0, eq) : body;
    const val = eq >= 0 ? body.slice(eq + 1) : '';
    const inner = page.locator(tid(id) + ' input').first();
    const loc = (await inner.count()) ? inner : page.locator(tid(id)).first();
    await loc.fill(val).catch(() => {});
    await page.keyboard.press('Enter').catch(() => {});
    return true;
  }
  const t = spec.startsWith('click:testid:') ? spec.slice('click:testid:'.length)
    : spec.startsWith('testid:') ? spec.slice('testid:'.length) : null;
  if (t) { await page.locator(tid(t)).first().click({ force: true }).catch(() => {}); return true; }
  const tab = page.locator('.t-tabs__nav-item', { hasText: spec }).first();
  if (await tab.count()) { await tab.click({ force: true }).catch(() => {}); return true; }
  return false;
}

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
const bandsOut = {};

// 三条「带」：本批所有改动都落在这三种元素里
const BANDS = [
  ['toolband', '.tool-row'],
  ['headband', '.panel-head'],
  ['pager', '.list-pager, .list-card__pager'],
];

for (const route of ROUTES) {
  await page.goto('http://127.0.0.1:' + PORT + route, { waitUntil: 'networkidle', timeout: 45000 }).catch(() => {});
  await page.waitForTimeout(900);
  for (const spec of PRE_CLICKS[route] || []) {
    const clicked = await doAction(page, spec);
    if (clicked) await page.waitForTimeout(1200);
  }
  await page.addStyleTag({ content: '*,*::before,*::after{transition:none!important;animation:none!important;caret-color:transparent!important}' });
  await page.waitForTimeout(250);
  const base = slug(route);
  manifest[route] = path.join(OUT, base + '.png');
  await page.screenshot({ path: manifest[route], animations: 'disabled', caret: 'hide' });
  for (const [name, sel] of BANDS) {
    // 逐个把「带」滚进视口再量框：分页条这类元素常在首屏之下，不滚就裁不到（前后两侧同样处理 → 仍可比）
    const boxes = await page.evaluate(async (s) => {
      const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
      const out = [];
      for (const el of Array.from(document.querySelectorAll(s)).slice(0, 3)) {
        el.scrollIntoView({ block: 'center', behavior: 'instant' });
        await sleep(120);
        const r = el.getBoundingClientRect();
        out.push({ y: Math.max(0, Math.round(r.y) - 8), h: Math.round(r.height) + 16 });
      }
      return out;
    }, sel).catch(() => []);
    if (!boxes.length) continue;
    for (let i = 0; i < boxes.length; i++) {
      const b = boxes[i];
      const h = Math.min(Math.max(b.h, 24), 1000 - b.y);
      if (!(h > 8) || b.y >= 1000) continue; // 裁切退化 → 跳过（viewport 图已覆盖）
      const f = path.join(OUT, `${base}.${name}${i ? i + 1 : ''}.png`);
      await page.screenshot({ path: f, clip: { x: 0, y: b.y, width: WIDTH, height: h }, animations: 'disabled', caret: 'hide' }).catch(() => {});
      (bandsOut[route] = bandsOut[route] || []).push(f);
    }
  }
  await page.evaluate(() => window.scrollTo(0, 0)).catch(() => {});
  console.log(`${route} → ${manifest[route]}（带 ${(bandsOut[route] || []).length} 张）`);
}
fs.writeFileSync(path.join(OUT, 'manifest.json'), JSON.stringify({ width: WIDTH, shots: manifest, bands: bandsOut }, null, 1));
await browser.close();
server.close();
console.log('共 ' + Object.keys(manifest).length + ' 页');
