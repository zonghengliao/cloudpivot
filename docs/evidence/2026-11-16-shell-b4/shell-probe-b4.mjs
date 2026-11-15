// 壳类逐元素计算样式 + 几何探针（B3 版；B1 卡 t_79f92c08 起、B2 t_9c90f3d1 沿用）
//
// 用法（必须在 frontend/ 下跑，`@playwright/test` 才可解析）：
//   node ../docs/evidence/2026-11-16-shell-b3/shell-probe-b3.mjs --dist <dist> --out <json> \
//        [--routes routes.txt] [--perms all|narrow]
//
// 与 B2 的差异只有两点：
//   ① 路由清单换成 B3 的 22 条采集项（13 个改动页 + 8 个基线爆炸半径页 + 3 个 hint--end 对照页）；
//   ② 新增 `--perms narrow`：`hr/employee` 的 `panel-head__hint` 只在「无建档权限」时渲染
//      （`v-if="!canCreate"`），默认超管桩永远看不到它 —— 用它把那个元素真正采出来。
//
// 口径（照抄 docs/evidence/2026-11-16-shell-style-sink/shell-sink-evidence.md §3.1）：
//   · 静态 dist + **全量 API 打桩**（同一份桩跑前后两遍 ⇒ DOM 输入完全相同，只有 CSS 可能变）；
//   · 每个「带壳类」元素 dump 40 个计算属性 + getBoundingClientRect；
//   · 元素身份 = DOM 路径（tag:nth-child 链），前后按路径对齐；
//   · 对照组：同一份 dist 连跑两遍必须 0 差异（排除字体/异步噪声），否则探针不算证据。
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
const arg = (k, d = null) => {
  const i = argv.indexOf('--' + k);
  return i >= 0 ? argv[i + 1] : d;
};
const DIST = path.resolve(arg('dist'));
const OUT = path.resolve(arg('out'));
const ROUTES_FILE = arg('routes');
const PERMS = arg('perms', 'all');
// B4 新增：视口宽度（本批 `tool-card`/`list-card` 的 flex 归属要看窄窗会不会被挤扁 → 1280 + 1600 两档）
const WIDTH = Number(arg('width', '1600'));
const PORT = Number(arg('port', '5219'));
if (!['all', 'narrow'].includes(PERMS)) throw new Error('--perms 只支持 all|narrow');

const HERE = path.dirname(new URL(import.meta.url).pathname);
const DEFAULT_ROUTES = fs
  .readFileSync(path.join(HERE, 'routes.txt'), 'utf8')
  .split('\n')
  .map((s) => s.trim())
  .filter((s) => s && !s.startsWith('#'));
const ROUTES = ROUTES_FILE
  ? fs.readFileSync(ROUTES_FILE, 'utf8').split('\n').map((s) => s.trim()).filter((s) => s && !s.startsWith('#'))
  : DEFAULT_ROUTES;

// 有些壳元素在非默认 Tab / 非默认步骤里（TDesign tabs 懒渲染），探针得先点开。
const PRE_CLICKS = {
  '/business/erp/ledger?probe=trial': ['科目余额表'],
  '/business/erp/cost-period': ['testid:cp-btn-new', 'testid:cp-btn-next'],
  // hr/salary 的 `panel-head`/`panel-head__title`（「逐人核对」）在试算抽屉里，
  // 得先开抽屉再点「跑试算」（`/salary/calc` 用具形状桩）
  '/business/hr/salary': ['testid:hr-salary-preview', 'testid:hr-salary-calc-run'],
  // receivable 的 tool-card/list-card 在「账龄看板」Tab（B4 两个块都在那里）
  '/business/erp/receivable': ['账龄看板'],
};

const MIME = {
  '.html': 'text/html; charset=utf-8', '.js': 'text/javascript; charset=utf-8', '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8', '.json': 'application/json; charset=utf-8', '.svg': 'image/svg+xml',
  '.png': 'image/png', '.jpg': 'image/jpeg', '.woff': 'font/woff', '.woff2': 'font/woff2', '.ttf': 'font/ttf', '.ico': 'image/x-icon',
};

const PAGE_DATA = { list: [], total: 0, page: 1, page_size: 20 };
const ok = (data) => JSON.stringify({ code: 0, msg: 'ok', data });

// narrow 档：只给 HR 员工页的「看」权限，故意不给 business:hr:employee:create，
// 让 `panel-head__hint` 渲染出来（本批要改它的 max-width/line-height/text-align）。
const PERMS_ALL = ['*:*:*'];
const PERMS_NARROW = [
  'business:hr:employee:page', 'business:hr:employee:list', 'business:hr:employee:detail',
  'business:hr:attendance:page', 'business:hr:leave:page', 'business:hr:salary:page',
  'business:erp:warehouse:list', 'business:erp:asset:list',
];

function stub(reqUrl) {
  const p = new URL(reqUrl, 'http://127.0.0.1').pathname;
  if (p === '/api/system/auth/me') {
    return ok({
      id: 'aaaaaaaa-0000-0000-0000-000000000001', username: 'shell_probe', real_name: '壳探针',
      email: 'probe@example.com', mobile: '', dept_id: null,
      roles: PERMS === 'all' ? ['SUPER_ADMIN'] : ['EMPLOYEE'],
      permissions: PERMS === 'all' ? PERMS_ALL : PERMS_NARROW,
      must_change_password: false,
    });
  }
  if (p === '/api/system/auth/menus') {
    return ok([{
      id: 'm-dir', name: 'erp', title: '企业管理', path: '/business/erp', component: 'Layout', icon: 'app',
      menu_type: 1, auth_code: '', parent_id: null, sort_order: 10, is_visible: true,
      children: [
        { id: 'm-asset', name: 'erp-admin-asset', title: '资产管理', path: '/business/erp/asset', component: 'business/erp/asset/index', icon: 'money', menu_type: 2, auth_code: 'business:erp:asset:list', parent_id: 'm-dir', sort_order: 10, is_visible: true, children: [] },
        { id: 'm-contract', name: 'erp-contract', title: '合同管理', path: '/business/erp/contract', component: 'business/erp/contract/index', icon: 'file', menu_type: 2, auth_code: 'business:erp:contract:list', parent_id: 'm-dir', sort_order: 20, is_visible: true, children: [] },
      ],
    }]);
  }
  if (p === '/api/system/auth/idle-policy') return ok({ timeout_minutes: 0, warn_seconds: 0, is_super_admin: PERMS === 'all' });
  if (p === '/api/system/auth/refresh') return ok({ access_token: 'probe-token', refresh_token: 'probe-refresh', token_type: 'Bearer', expires_in: 86400 });
  if (p.startsWith('/api/support/notice') || p.startsWith('/api/system/notice')) return ok([]);
  if (p.includes('/dict/')) return ok([]);
  if (p.includes('/dept/tree')) return ok([]);
  if (p.includes('/switch')) return ok({ enabled: false, cost_enabled: false, inventory_enabled: false });
  // 具形状桩（空表桩会让这两页在 render 期抛错，整页采不到）
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
  // hr/salary 试算（`POST /salary/calc`）：具形状，否则抽屉里 `calcResult.totals.employee_count` 直接抛
  if (p.includes('/hr/salary/calc') || p.endsWith('/hr/salary')) {
    const emp = (no, name, dept) => ({
      employee_id: 'emp-' + no, employee_no: no, employee_name: name, dept_name: dept,
      should_days: '21.75', actual_days: '21.75', leave_unpaid_days: '0', ot_hours: '0',
      earning_amount: '12000.00', pre_deduct_amount: '0.00', gross_amount: '12000.00',
      withhold_amount: '1500.00', net_amount: '10500.00',
    });
    return ok({
      period_code: '2026-11', period_start: '2026-11-01', period_end: '2026-11-30',
      attendance_period_id: null, dry_run: true, sheet: null,
      totals: {
        employee_count: 2, earning_total: '24000.00', pre_deduct_total: '0.00', gross_total: '24000.00',
        withhold_total: '3000.00', net_total: '21000.00', si_company_total: '3000.00',
        hf_company_total: '1500.00', accrual_total: '28500.00',
      },
      item_count: 8, missing_attendance: [],
      employees: [emp('E0001', '张三', '财务部'), emp('E0002', '李四', '生产部')],
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

const SHELL_SEL = '[class*="panel-head"], [class*="panel-empty"], [class*="tool-row"], [class*="tool-card"], [class*="list-card"], [class*="lp__filter-row"]';

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
      // 卡片头的直接子元素（B4 用：判断 wrap 是否真的换行、align-items 差异落到哪个子元素）
      let kids = null;
      if (/panel-head(\s|$)/.test(el.getAttribute('class') || '')) {
        kids = Array.prototype.map.call(el.children, (c) => {
          const cr = c.getBoundingClientRect();
          return {
            cls: c.getAttribute('class') || '',
            text: (c.textContent || '').trim().slice(0, 20),
            rect: { x: Math.round(cr.x * 10) / 10, y: Math.round(cr.y * 10) / 10, w: Math.round(cr.width * 10) / 10, h: Math.round(cr.height * 10) / 10 },
          };
        });
      }
      out.push({
        path: cssPath(el),
        tag: el.tagName.toLowerCase(),
        cls: el.getAttribute('class') || '',
        text: (el.textContent || '').trim().slice(0, 40),
        rect: { x: Math.round(r.x * 10) / 10, y: Math.round(r.y * 10) / 10, w: Math.round(r.width * 10) / 10, h: Math.round(r.height * 10) / 10 },
        // 溢出探针：nowrap 把换行换成溢出的场景（基线新增 nowrap 后必须为 0）
        overflow: { sw: el.scrollWidth, cw: el.clientWidth, sh: el.scrollHeight, ch: el.clientHeight },
        kids,
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
    localStorage.setItem('cp-onboarding-done', '1'); // 否则首屏浮新功能引导卡，挡住点击
    localStorage.setItem('cloudpivot.setup-wizard.skipped:shell_probe', '1'); // 首次启动向导（关键角色 0 人）会盖住页面中部
  });
  const page = await ctx.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e).slice(0, 160)));

  const result = { dist: DIST, perms: PERMS, width: WIDTH, routes: {} };
  for (const route of ROUTES) {
    errors.length = 0;
    await page.goto('http://127.0.0.1:' + PORT + route, { waitUntil: 'networkidle', timeout: 45000 }).catch(() => {});
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
    // 排障用：--sel 给一个选择器，逐路由打印匹配元素的属性与关键计算值
    const SEL = arg('sel', null);
    if (SEL) {
      const extra = await page.evaluate((sel) => Array.from(document.querySelectorAll(sel)).map((el) => {
        const cs = getComputedStyle(el);
        return {
          attrs: Array.from(el.attributes).map((a) => a.name + '=' + a.value.slice(0, 80)),
          display: cs.getPropertyValue('display'),
          alignItems: cs.getPropertyValue('align-items'),
          gap: cs.getPropertyValue('gap'),
          rect: { x: Math.round(el.getBoundingClientRect().x), y: Math.round(el.getBoundingClientRect().y), w: Math.round(el.getBoundingClientRect().width) },
        };
      }), SEL).catch(() => []);
      if (extra.length) console.log('   SEL ' + SEL + ': ' + JSON.stringify(extra));
    }
    result.routes[route] = { url: page.url(), count: els.length, errors: [...errors], elements: els };
    console.log(`${route}  →  ${els.length} 元素${errors.length ? '  页面错误 ' + errors.length : ''}`);
  }
  fs.writeFileSync(OUT, JSON.stringify(result, null, 1));
  console.log('→ ' + OUT);
  await browser.close();
  server.close();
};

main().catch((e) => { console.error(e); process.exit(1); });
