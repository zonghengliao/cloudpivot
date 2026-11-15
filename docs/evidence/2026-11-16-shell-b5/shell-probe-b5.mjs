// 壳类逐元素计算样式 + 几何探针（B5 版，基于 B4 的 shell-probe-b4.mjs）
//
// 用法（必须在 frontend/ 下跑，`@playwright/test` 才可解析）：
//   node ../docs/evidence/2026-11-16-shell-b5/shell-probe-b5.mjs --dist <dist> --out <json> \
//        [--routes routes.txt] [--width 1600]
//
// B5 的口径：
//   · 采集选择器**只含** `[class*="panel-empty"]` 与 `[class*="tool-row__label"]`（本批的类）；
//   · 每页多采一层「直接文本节点」标记（`hasOwnText`）—— `hr/salary/detail` 的内嵌空态是纯文字，
//     `panel-empty.font-size` 只对它们有几何意义；
//   · `__desc` 额外算 `lines`（clientHeight / line-height）—— 卡面点名要盯的行数；
//   · 空表桩（`/api/**` 返空列表）让空态渲染出来。
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
const PORT = Number(arg('port', '5219'));

const HERE = path.dirname(new URL(import.meta.url).pathname);
const DEFAULT_ROUTES = path.join(HERE, 'routes.txt');
const ROUTES = (ROUTES_FILE || DEFAULT_ROUTES) && fs
  .readFileSync(ROUTES_FILE || DEFAULT_ROUTES, 'utf8')
  .split('\n').map((s) => s.trim()).filter((s) => s && !s.startsWith('#'));

// 非默认 Tab / 抽屉里的空态（TDesign tabs 懒渲染，得先点开才在 DOM 里）
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
const ok = (data) => JSON.stringify({ code: 0, msg: 'ok', data });

// 报表/详情类接口需要「具形状」的空数据，否则页面在 render 期抛错，整页采不到。
const REPORT_STUB = {
  report_code: 'PROBE', report_name: '壳探针报表', company_name: '壳探针', unit: '元',
  as_of: '2026-11-16', as_of_source: 'TODAY', period: null, period_name: null, period_status: null,
  period_start: '2026-11-01', period_end: '2026-11-30',
  generated_at: '2026-11-16 00:00:00', buckets: [], summary: [], total_unpaid: '0', overdue_unpaid: '0',
  overdue_ratio: '0', total_amount: '0', total_paid: '0', doc_count: 0, supplier_count: 0,
  suppliers: [], details: [], detail_count: 0, detail_truncated: false, hints: [],
  rows: [], sections: [], items: [], lines: [], columns: [], balances: [],
  total_debit: '0', total_credit: '0', total_opening_debit: '0', total_opening_credit: '0',
  total_period_debit: '0', total_period_credit: '0', total_closing_debit: '0', total_closing_credit: '0',
  balanced: true, opening_balance: '0', closing_balance: '0', children: [],
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
        { id: 'm-asset', name: 'erp-admin-asset', title: '资产管理', path: '/business/erp/asset', component: 'business/erp/asset/index', icon: 'money', menu_type: 2, auth_code: 'business:erp:asset:list', parent_id: 'm-dir', sort_order: 10, is_visible: true, children: [] },
      ],
    }]);
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
    return ok({
      period_code: '2026-11', period_start: '2026-11-01', period_end: '2026-11-30',
      attendance_period_id: null, dry_run: true, sheet: null, item_count: 8, missing_attendance: [],
      totals: { employee_count: 0, earning_total: '0', pre_deduct_total: '0', gross_total: '0', withhold_total: '0', net_total: '0', si_company_total: '0', hf_company_total: '0', accrual_total: '0' },
      employees: [],
    });
  }
  // 薪资单详情（hr/salary/:id）：空明细
  if (/\/hr\/salary\/[^/]+$/.test(p) && !p.endsWith('/page')) {
    return ok({
      id: 'bbbbbbbb-0000-0000-0000-000000000001', sheet_no: 'SAL-PROBE', period_code: '2026-11',
      status: 'DRAFT', status_name: '草稿', dept_summary: [], detail_count: 0,
      employee_count: 0, net_total: '0', gross_total: '0', approval_instance_id: null,
      period_start: '2026-11-01', period_end: '2026-11-30', created_at: '2026-11-16 00:00:00',
      can_edit: true, can_submit: true, can_post: false, can_reverse: false,
    });
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

const SHELL_SEL = '[class*="panel-empty"], [class*="tool-row__label"]';

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
      const hasOwnText = Array.prototype.some.call(el.childNodes, (n) => n.nodeType === 3 && (n.textContent || '').trim());
      const lh = parseFloat(cs.getPropertyValue('line-height'));
      out.push({
        path: cssPath(el),
        tag: el.tagName.toLowerCase(),
        cls: el.getAttribute('class') || '',
        text: (el.textContent || '').trim().slice(0, 60),
        rect: { x: Math.round(r.x * 10) / 10, y: Math.round(r.y * 10) / 10, w: Math.round(r.width * 10) / 10, h: Math.round(r.height * 10) / 10 },
        overflow: { sw: el.scrollWidth, cw: el.clientWidth, sh: el.scrollHeight, ch: el.clientHeight },
        hasOwnText,
        // `__desc` 的行数：高度 / 行高（卡面点名要盯）
        lines: lh ? Math.round((r.height / lh) * 100) / 100 : null,
        style,
      });
    }
    return out;
  }, { props: PROPS, sel: SHELL_SEL });
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
      const clicked = await page.evaluate((spec) => {
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
    const pe = els.filter((e) => /panel-empty/.test(e.cls)).length;
    console.log(`${route}  →  ${els.length} 元素（panel-empty ${pe}）${errors.length ? '  页面错误 ' + errors.length + ': ' + errors[0] : ''}`);
  }
  fs.writeFileSync(OUT, JSON.stringify(result, null, 1));
  console.log('→ ' + OUT);
  await browser.close();
  server.close();
};

main().catch((e) => { console.error(e); process.exit(1); });
