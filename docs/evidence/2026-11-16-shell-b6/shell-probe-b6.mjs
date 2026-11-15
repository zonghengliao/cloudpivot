// 壳类逐元素计算样式 + 几何探针（B6 版，基于 B5 的 shell-probe-b5.mjs / B4 的 shell-probe-b4.mjs）
//
// 用法（必须在 frontend/ 下跑，`@playwright/test` 才可解析）：
//   node ../docs/evidence/2026-11-16-shell-b6/shell-probe-b6.mjs --dist <dist> --out <json> [--width 1600]
//
// B6 的口径与 B5 的差别只有一处，但很关键：**采集面是"所有带 class 的元素"**（`[class]`），
// 而不是"命中壳类前缀的元素"。原因：本批 C 组是**类名替换**（`.tool-row__switch` → `.inv-only-toggle`），
// 按壳前缀抓选择器会在 after 侧抓不到元素，等于自证。按 CSS 路径（tag:nth-child 链）对齐元素身份后，
// 改名前后必须是「class 属性变、40 个计算属性 + 几何全同」才算过。
//
// 其余口径照抄 docs/evidence/2026-11-16-shell-style-sink/shell-sink-evidence.md §3.1：
//   · 静态 dist + 全量 API 打桩（同一份桩跑前后两遍 ⇒ DOM 输入完全相同，只有 CSS/类名可能变）；
//   · 每元素 dump 40 个计算属性 + getBoundingClientRect + overflow；
//   · 对照组：同一份 dist 连跑两遍必须 0 差异。
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';

const req = createRequire(path.join(process.cwd(), '__resolve__.js'));
function loadPlaywright() {
  for (const name of ['@playwright/test', 'playwright']) {
    try { return req(name).chromium; } catch { /* next */ }
  }
  throw new Error('找不到 @playwright/test：请先 cd frontend && pnpm install，并在 frontend/ 目录下跑本脚本');
}
const chromium = loadPlaywright();

const argv = process.argv.slice(2);
const arg = (k, d = null) => { const i = argv.indexOf('--' + k); return i >= 0 ? argv[i + 1] : d; };
const DIST = path.resolve(arg('dist'));
const OUT = path.resolve(arg('out'));
const ROUTES_FILE = arg('routes');
const WIDTH = Number(arg('width', '1600'));
const PORT = Number(arg('port', '5223'));

const HERE = path.dirname(new URL(import.meta.url).pathname);
const ROUTES = fs
  .readFileSync(ROUTES_FILE || path.join(HERE, 'routes.txt'), 'utf8')
  .split('\n').map((s) => s.trim()).filter((s) => s && !s.startsWith('#'));

// 有些目标元素在非默认 Tab / 非默认步骤 / 需要开抽屉 / 需要用户先选一次下拉（TDesign 懒渲染，
// 且不少壳元素挂在 `v-if` 上），探针得先动手。动作语法：
//   `testid:x`        点 `[data-testid="x"]`
//   `click:testid:x`  同上（显式写法，便于与 testid 直点区分）
//   `fill:testid:x=V` 把 x 填成 V 并派发 input 事件（v-model 生效）
//   `pick:testid:x=T` 点开下拉 x，再点文案含 T 的选项
//   其它              点同名 tab
const PRE_CLICKS = {
  // UncollectedPanel（`.panel-head__ops` → `panel-head__side`）在向导步 ②，得先建单再进下一步
  '/business/erp/cost-period': ['testid:cp-btn-new', 'testid:cp-btn-next'],
  // 总账的 `.panel-head__account` 是 `v-if="currentAccountText"`：不选科目不渲染
  '/business/erp/ledger': ['pick:testid:ledger-filter-account=1001'],
  // 序列号的 `.panel-head__warn` 是 `v-if="scannedId && !scanHitOnPage"`：得真扫一次（扫到的 id 不在本页）
  '/business/erp/serial': ['scan:testid:serial-scan-input=SN-PROBE-1'],
};

const MIME = {
  '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8', '.json': 'application/json; charset=utf-8', '.svg': 'image/svg+xml',
  '.png': 'image/png', '.jpg': 'image/jpeg', '.woff': 'font/woff', '.woff2': 'font/woff2', '.ttf': 'font/ttf', '.ico': 'image/x-icon',
};

const PAGE_DATA = { list: [], total: 0, page: 1, page_size: 20 };
const ok = (data) => JSON.stringify({ code: 0, msg: 'ok', data });

// 报表/详情类接口需要「具形状」的空数据，否则页面在 render 期抛错，整页采不到。
const REPORT_STUB = {
  report_code: 'PROBE', report_name: '壳探针报表', company_name: '壳探针', unit: '元',
  as_of: '2026-11-16', as_of_source: 'TODAY', period: null, period_name: null, period_status: null,
  period_start: '2026-11-01', period_end: '2026-11-30',
  generated_at: '2026-11-16 00:00:00', buckets: [], summary: [], total_unpaid: '0', overdue_unpaid: '0',
  overdue_ratio: '0', total_amount: '0', total_paid: '0', doc_count: 0, supplier_count: 0,
  suppliers: [], details: [], detail_count: 0, detail_truncated: false, hints: [],
  rows: [], sections: [], items: [], lines: [], columns: [], balances: [], slots: [], months: [],
  total_debit: '0', total_credit: '0', total_opening_debit: '0', total_opening_credit: '0',
  total_period_debit: '0', total_period_credit: '0', total_closing_debit: '0', total_closing_credit: '0',
  balanced: true, opening_balance: '0', closing_balance: '0', children: [],
};

// 帮助中心：`.list-card__pager` 只在 `total > pageSize` 时渲染 → 必须给 30 篇
const HELP_DOCS = {
  list: Array.from({ length: 12 }, (_, i) => ({
    id: 'cccccccc-0000-0000-0000-' + String(i + 1).padStart(12, '0'),
    title: '探针文档 ' + (i + 1), summary: '用于壳探针的分页样本',
    category_id: 'cat-1', category_name: '快速上手', status: 'PUBLISHED',
    updated_at: '2026-11-16 00:00:00', read_count: 0, version: 1,
  })),
  total: 30, page: 1, page_size: 12,
};
const HELP_TREE = [{
  id: 'cat-1', name: '快速上手', parent_id: null, sort_order: 10, doc_count: 30, children: [],
}];

// 调拨页头角标（`.panel-head__overdue`）只在 summary.total > 0 时渲染
const TRANSFER_OVERDUE = { threshold_days: 7, total: 3, oldest_days: 12 };
// 序列号扫码（`.panel-head__warn`）：扫到的 id 不在当前页 → 提示条渲染
const SERIAL_SCAN = {
  id: 'dddddddd-0000-0000-0000-000000000001', serial_no: 'SN-PROBE-1', status: 'IN_STOCK',
  material_id: 'eeeeeeee-0000-0000-0000-000000000001', material_code: 'M-001', material_name: '探针物料',
  warehouse_id: null, warehouse_name: '', location_id: null, location_name: '',
  inbound_at: '2026-11-01 00:00:00', outbound_at: null, doc_no: '',
};
// 总账科目下拉（选一个才能渲染 `.panel-head__account`）
const ACCOUNT_PAGE = {
  list: [{
    id: 'ffffffff-0000-0000-0000-000000000001', code: '1001', name: '库存现金',
    category: 'ASSET', direction: 'DEBIT', is_leaf: true, status: 1, parent_id: null,
  }],
  total: 1, page: 1, page_size: 20,
};

function stub(reqUrl) {
  const p = new URL(reqUrl, 'http://127.0.0.1').pathname;
  if (p === '/api/system/auth/me') {
    return ok({
      id: 'aaaaaaaa-0000-0000-0000-000000000001', username: 'shell_probe', real_name: '壳探针',
      email: 'probe@example.com', mobile: '', dept_id: null,
      roles: ['SUPER_ADMIN'], permissions: ['*:*:*'], must_change_password: false,
    });
  }
  if (p === '/api/system/auth/menus') {
    return ok([{
      id: 'm-dir', name: 'erp', title: '企业管理', path: '/business/erp', component: 'Layout', icon: 'app',
      menu_type: 1, auth_code: '', parent_id: null, sort_order: 10, is_visible: true,
      children: [
        { id: 'm-acc', name: 'erp-account', title: '科目', path: '/business/erp/account', component: 'business/erp/account/index', icon: 'money', menu_type: 2, auth_code: 'business:erp:account:list', parent_id: 'm-dir', sort_order: 10, is_visible: true, children: [] },
        { id: 'm-help', name: 'help-center', title: '帮助中心', path: '/help', component: 'business/help/center/index', icon: 'help', menu_type: 2, auth_code: 'system:help:read', parent_id: 'm-dir', sort_order: 20, is_visible: true, children: [] },
      ],
    }]);
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

const PROPS = [
  'display', 'flex', 'flex-grow', 'flex-shrink', 'flex-basis', 'flex-direction', 'flex-wrap',
  'align-items', 'justify-content', 'gap', 'row-gap', 'column-gap',
  'margin-top', 'margin-right', 'margin-bottom', 'margin-left',
  'padding-top', 'padding-right', 'padding-bottom', 'padding-left',
  'font-size', 'font-weight', 'font-style', 'color', 'line-height', 'letter-spacing',
  'white-space', 'text-align', 'text-overflow', 'vertical-align',
  'width', 'min-width', 'max-width', 'height', 'min-height',
  'box-sizing', 'border-radius', 'border-top-width', 'background-color', 'position',
  'cursor', 'filter', 'text-decoration-line',
];

// B6：全量采集（所有带 class 的元素 + `<code>` —— inventory 巡检口径里的行内代码块没有 class，
// 本批要给它挂页面私有 token，before 侧只能按标签抓）。改名前后靠 CSS 路径对齐，不靠类名。
const SHELL_SEL = '[class], code';
const CAP = 5000;

async function collect(page) {
  return page.evaluate(({ props, sel, cap }) => {
    const cssPath = (el) => {
      const parts = [];
      let cur = el;
      while (cur && cur.nodeType === 1 && cur !== document.body) {
        const parent = cur.parentElement;
        if (!parent) break;
        parts.unshift(cur.tagName.toLowerCase() + ':nth-child(' + (Array.prototype.indexOf.call(parent.children, cur) + 1) + ')');
        cur = parent;
      }
      return parts.join('>');
    };
    const out = [];
    const all = document.querySelectorAll(sel);
    for (const el of all) {
      if (out.length >= cap) break;
      const cs = getComputedStyle(el);
      const style = {};
      for (const p of props) style[p] = cs.getPropertyValue(p);
      const r = el.getBoundingClientRect();
      out.push({
        path: cssPath(el),
        tag: el.tagName.toLowerCase(),
        cls: el.getAttribute('class') || '',
        text: (el.textContent || '').trim().slice(0, 40),
        rect: { x: Math.round(r.x * 10) / 10, y: Math.round(r.y * 10) / 10, w: Math.round(r.width * 10) / 10, h: Math.round(r.height * 10) / 10 },
        overflow: { sw: el.scrollWidth, cw: el.clientWidth, sh: el.scrollHeight, ch: el.clientHeight },
        style,
      });
    }
    return { elements: out, total: all.length };
  }, { props: PROPS, sel: SHELL_SEL, cap: CAP });
}

const tid = (id) => '[data-testid="' + id + '"]';

// 动作执行器（跑在 Node 侧，用 Playwright 的真实交互而不是 dispatchEvent ——
// TDesign 的 select 要 mousedown/click 才开浮层，`t-input` 的 testid 在内层 input 上）
async function doAction(page, spec) {
  if (spec.startsWith('fill:testid:')) {
    const head = spec.slice('fill:testid:'.length);
    const eq = head.indexOf('=');
    const id = eq >= 0 ? head.slice(0, eq) : head;
    const val = eq >= 0 ? head.slice(eq + 1) : '';
    const loc = page.locator(tid(id) + ' input, ' + tid(id) + ' textarea, ' + tid(id)).first();
    await loc.fill(val).catch(() => {});
    return true;
  }
  if (spec.startsWith('pick:testid:')) {
    const body = spec.slice('pick:testid:'.length);
    const eq = body.indexOf('=');
    const id = eq >= 0 ? body.slice(0, eq) : body;
    const want = eq >= 0 ? body.slice(eq + 1) : '';
    // TDesign select 对合成 click 不买账（实测 dispatchEvent / mouse.click / force click 都开不出浮层），
    // 但「聚焦 input → ArrowDown」一定开，且第一条就是首选值
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
    // 扫码框：填值 + 回车（模板上是 `@enter="handleScan"`；按钮走 `click:testid:`）
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
  if (t) {
    await page.locator(tid(t)).first().click({ force: true }).catch(() => {});
    return true;
  }
  const tab = page.locator('.t-tabs__nav-item', { hasText: spec }).first();
  if (await tab.count()) { await tab.click({ force: true }).catch(() => {}); return true; }
  return false;
}

const main = async () => {
  await new Promise((r) => server.listen(PORT, '127.0.0.1', r));
  const browser = await chromium.launch();
  const ctx = await browser.newContext({ viewport: { width: WIDTH, height: 1000 }, locale: 'zh-CN', timezoneId: 'Asia/Shanghai' });
  await ctx.addInitScript(() => {
    localStorage.setItem('x_access_token', 'probe-token');
    localStorage.setItem('x_refresh_token', 'probe-refresh');
    localStorage.setItem('cp-onboarding-done', '1');
    localStorage.setItem('cloudpivot.setup-wizard.skipped:shell_probe', '1');
  });
  const page = await ctx.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e).slice(0, 160)));

  const result = { dist: DIST, width: WIDTH, routes: {} };
  for (const route of ROUTES) {
    errors.length = 0;
    await page.goto('http://127.0.0.1:' + PORT + route, { waitUntil: 'networkidle', timeout: 45000 }).catch(() => {});
    await page.waitForTimeout(900);
    for (const label of PRE_CLICKS[route] || []) {
      const clicked = await doAction(page, label);
      if (clicked) await page.waitForTimeout(1200);
    }
    let got = { elements: [], total: 0 };
    try { got = await collect(page); } catch { /* keep empty */ }
    result.routes[route] = { url: page.url(), count: got.elements.length, domTotal: got.total, errors: [...errors], elements: got.elements };
    console.log(`${route}  →  ${got.elements.length}/${got.total} 元素${errors.length ? '  页面错误 ' + errors.length + ': ' + errors[0] : ''}`);
  }
  fs.writeFileSync(OUT, JSON.stringify(result, null, 1));
  console.log('→ ' + OUT);
  await browser.close();
  server.close();
};

main().catch((e) => { console.error(e); process.exit(1); });
