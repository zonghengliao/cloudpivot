// 壳类逐元素计算样式 + 几何探针（B1 卡 t_79f92c08 验收第 4 条；B2–B6 直接复用）
//
// 用法：
//   node shell-probe.mjs --dist <dist 目录> --out <json> [--routes routes.txt]
//
// 口径（照抄 docs/evidence/2026-11-16-shell-style-sink/shell-sink-evidence.md §3.1）：
//   · 静态 dist + **全量 API 打桩**（同一份桩跑前后两遍 ⇒ DOM 输入完全相同，只有 CSS 可能变）；
//   · 每个「带壳类」元素 dump 40 个计算属性 + getBoundingClientRect；
//   · 元素身份 = DOM 路径（tag:nth-child 链），前后按路径对齐；
//   · 对照组：同一份 dist 连跑两遍必须 0 差异（排除字体/异步噪声），否则探针不算证据。
//
// 为什么打桩而不是连真后端：本机没有可用后端（dev/prod 账号都不通用），而**前后对比**
// 只需要「同样的输入」，打桩比真后端更确定（父卡连的是 prod 只读库，本卡用打桩拿到同样的结论）。
//
// 注意：必须从 `frontend/` 目录下跑（`@playwright/test` 是 frontend 的直接依赖）。
//   cd frontend && node ../docs/evidence/2026-11-16-shell-b1/shell-probe.mjs --dist dist --out /tmp/after.json
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';

// 本脚本住在 docs/evidence/ 下（不在 frontend/ 里），ESM 的裸包名解析会从**脚本自身位置**
// 开始找 node_modules —— 找不到。所以用 createRequire 从 **cwd**（约定在 frontend/ 下跑）解析。
const req = createRequire(path.join(process.cwd(), '__resolve__.js'));
function loadPlaywright() {
  for (const name of ['@playwright/test', 'playwright']) {
    try { return req(name).chromium; } catch { /* next */ }
  }
  throw new Error('找不到 @playwright/test：请先 cd frontend && pnpm install，并在 frontend/ 目录下跑本脚本');
}
const chromium = loadPlaywright();

const argv = process.argv.slice(2);
const arg = (k, d = null) => {
  const i = argv.indexOf('--' + k);
  return i >= 0 ? argv[i + 1] : d;
};
const DIST = path.resolve(arg('dist'));
const OUT = path.resolve(arg('out'));
const ROUTES_FILE = arg('routes');

// 默认 16 条 = B1 批受影响的全部路由（B2–B6 用自己的清单，用 --routes 传）
// B2 批（本文件）：24 页 / 27 条采集项（3 页多采一次非默认 Tab，见 PRE_CLICKS）
const DEFAULT_ROUTES = [
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
const ROUTES = ROUTES_FILE
  ? fs.readFileSync(ROUTES_FILE, 'utf8').split('\n').map((s) => s.trim()).filter((s) => s && !s.startsWith('#'))
  : DEFAULT_ROUTES;

// 有些壳元素在非默认 Tab / 非默认步骤里（TDesign tabs 懒渲染），探针得先点开。
// 值形如 '账龄看板'（按页签文字点）或 'testid:cp-btn-goto-check'（按 data-testid 点）。
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
const ok = (data) => JSON.stringify({ code: 0, msg: 'ok', data });

function stub(req) {
  const p = new URL(req.url, 'http://127.0.0.1').pathname;
  if (p === '/api/system/auth/me') {
    return ok({
      id: 'aaaaaaaa-0000-0000-0000-000000000001', username: 'shell_probe', real_name: '壳探针',
      email: 'probe@example.com', mobile: '', dept_id: null,
      roles: ['SUPER_ADMIN'], permissions: ['*:*:*'], must_change_password: false,
    });
  }
  if (p === '/api/system/auth/menus') {
    return ok([
      {
        id: 'm-dir', name: 'erp', title: '企业管理', path: '/business/erp', component: 'Layout', icon: 'app',
        menu_type: 1, auth_code: '', parent_id: null, sort_order: 10, is_visible: true,
        children: [
          { id: 'm-asset', name: 'erp-admin-asset', title: '资产管理', path: '/business/erp/asset', component: 'business/erp/asset/index', icon: 'money', menu_type: 2, auth_code: 'business:erp:asset:list', parent_id: 'm-dir', sort_order: 10, is_visible: true, children: [] },
          { id: 'm-contract', name: 'erp-contract', title: '合同管理', path: '/business/erp/contract', component: 'business/erp/contract/index', icon: 'file', menu_type: 2, auth_code: 'business:erp:contract:list', parent_id: 'm-dir', sort_order: 20, is_visible: true, children: [] },
        ],
      },
    ]);
  }
  if (p === '/api/system/auth/idle-policy') return ok({ timeout_minutes: 0, warn_seconds: 0, is_super_admin: true });
  if (p === '/api/system/auth/refresh') return ok({ access_token: 'probe-token', refresh_token: 'probe-refresh', token_type: 'Bearer', expires_in: 86400 });
  if (p.startsWith('/api/support/notice') || p.startsWith('/api/system/notice')) return ok([]);
  if (p.includes('/dict/')) return ok([]);
  if (p.includes('/dept/tree')) return ok([]);
  if (p.includes('/switch')) return ok({ enabled: false, cost_enabled: false, inventory_enabled: false });
  // ---- B2 补的两个具形状桩（空表桩会让这两页在 render 期抛错，整页采不到） ----
  if (p.includes('/erp/ledger/trial-balance')) {
    return ok({
      period: '2026-11', period_start: '2026-11-01', period_end: '2026-11-30', rows: [],
      total_opening_debit: '0', total_opening_credit: '0', total_period_debit: '0', total_period_credit: '0',
      total_closing_debit: '0', total_closing_credit: '0', balanced: true,
    });
  }
  if (p.includes('/erp/report/payable-aging')) {
    return ok({
      report_code: 'PAYABLE_AGING', report_name: '应付账龄分析表', company_name: '壳探针', unit: '元',
      as_of: '2026-11-16', as_of_source: 'TODAY', period: null, period_name: null, period_status: null,
      generated_at: '2026-11-16 00:00:00', buckets: [], summary: [], total_unpaid: '0', overdue_unpaid: '0',
      overdue_ratio: '0', total_amount: '0', total_paid: '0', doc_count: 0, supplier_count: 0,
      suppliers: [], details: [], detail_count: 0, detail_truncated: false, hints: [],
    });
  }
  return ok(PAGE_DATA);
}

const server = http.createServer((req, res) => {
  const u = new URL(req.url, 'http://127.0.0.1');
  if (u.pathname.startsWith('/api/') || u.pathname === '/health') {
    res.writeHead(200, { 'content-type': 'application/json; charset=utf-8', 'cache-control': 'no-store' });
    res.end(stub(req));
    return;
  }
  let file = path.join(DIST, decodeURIComponent(u.pathname));
  if (!fs.existsSync(file) || fs.statSync(file).isDirectory()) file = path.join(DIST, 'index.html');
  res.writeHead(200, { 'content-type': MIME[path.extname(file).toLowerCase()] || 'application/octet-stream', 'cache-control': 'no-store' });
  fs.createReadStream(file).pipe(res);
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
];

const SHELL_SEL = '[class*="panel-head"], [class*="panel-empty"], [class*="tool-row"], [class*="tool-card"], [class*="list-card"]';

async function collect(page) {
  return page.evaluate(({ props, sel }) => {
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
    for (const el of document.querySelectorAll(sel)) {
      const cs = getComputedStyle(el);
      const style = {};
      for (const p of props) style[p] = cs.getPropertyValue(p);
      const r = el.getBoundingClientRect();
      out.push({
        path: cssPath(el),
        tag: el.tagName.toLowerCase(),
        cls: el.getAttribute('class') || '',
        rect: { x: Math.round(r.x * 10) / 10, y: Math.round(r.y * 10) / 10, w: Math.round(r.width * 10) / 10, h: Math.round(r.height * 10) / 10 },
        style,
      });
    }
    return out;
  }, { props: PROPS, sel: SHELL_SEL });
}

const main = async () => {
  await new Promise((r) => server.listen(5199, '127.0.0.1', r));
  const browser = await chromium.launch();
  const ctx = await browser.newContext({ viewport: { width: 1600, height: 1000 }, locale: 'zh-CN', timezoneId: 'Asia/Shanghai' });
  await ctx.addInitScript(() => {
    localStorage.setItem('x_access_token', 'probe-token');
    localStorage.setItem('x_refresh_token', 'probe-refresh');
    localStorage.setItem('cp-onboarding-done', '1'); // 否则首屏浮新功能引导卡，挡住点击
  });
  const page = await ctx.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e).slice(0, 160)));

  const result = { dist: DIST, routes: {} };
  for (const route of ROUTES) {
    errors.length = 0;
    await page.goto('http://127.0.0.1:5199' + route, { waitUntil: 'networkidle', timeout: 45000 }).catch(() => {});
    await page.waitForTimeout(900);
    for (const label of PRE_CLICKS[route] || []) {
      const clicked = await page.evaluate((spec) => {
        if (spec.startsWith('testid:')) {
          const el = document.querySelector(`[data-testid="${spec.slice(7)}"]`);
          if (!el) return false;
          el.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
          return true;
        }
        for (const it of document.querySelectorAll('.t-tabs__nav-item')) {
          if ((it.textContent || '').trim() === spec) {
            (it.querySelector('.t-tabs__nav-item-text-wrapper') || it).dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true }));
            return true;
          }
        }
        return false;
      }, label);
      if (clicked) await page.waitForTimeout(1200);
    }
    let els = [];
    try { els = await collect(page); } catch { els = []; }
    result.routes[route] = { url: page.url(), count: els.length, errors: [...errors], elements: els };
    console.log(`${route}  →  ${els.length} 元素${errors.length ? '  页面错误 ' + errors.length : ''}`);
  }
  fs.writeFileSync(OUT, JSON.stringify(result, null, 1));
  console.log('→ ' + OUT);
  await browser.close();
  server.close();
};

main().catch((e) => { console.error(e); process.exit(1); });
