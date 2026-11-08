/**
 * PDF-08 · 真实浏览器点击「下载 PDF」（Chromium / Playwright）
 *
 * 证明的是**前端这一层**真的能下下来：
 *   ① 各入口页面点开详情后，右上/吸顶动作条里有「下载 PDF」按钮
 *   ② 点下去真收到文件（`download` 事件），文件名是后端 Content-Disposition 的中文名
 *   ③ 文件是合法 PDF（体积 > 20KB）
 *   ④ 请求带 `Authorization: Bearer`（拦截请求头断言，不是 window.open）
 *   ⑤ 无权限账号点同一个按钮 → 403 + 顶部提示含「权限」
 *      （用报表页：`report:balance-sheet` 可看、`report:export` 未授予 —— 单据页的
 *        PDF 权限与「查看」同码，能看见单据就一定能导出，所以「看得见按钮但 403」
 *        的真实场景只在报表页出现）
 *   ⑥ 慢网络下按钮进 loading（拦截 PDF 请求延迟 1.5s 断言 loading class）
 *
 * 用法（在 99 上跑，前端用 vite dev 反代到 145 的隔离实例）：
 *   node docs/evidence/20261108-pdf08/browser_check.mjs
 * 环境变量：ADMIN_PW（默认 Admin@12345678）/ OUT（截图与下载落地目录）
 */
import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';
import { mkdirSync, statSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const OUT = process.env.OUT || '/opt/cloudpivot/docs/evidence/20261108-pdf08/sample';
const ADMIN_PW = process.env.ADMIN_PW || 'Admin@12345678';
const NOPERM_USER = 'pdf08_rpt_noperm';
const NOPERM_PW = 'Pdf08@123456';
mkdirSync(OUT, { recursive: true });

let PASS = 0;
let FAIL = 0;
const FAILS = [];
const LOG = [];

function check(cond, label, detail = '') {
  if (cond) {
    PASS += 1;
    LOG.push(`  [PASS] ${label}`);
    console.log(`  [PASS] ${label}`);
  } else {
    FAIL += 1;
    FAILS.push(`${label} :: ${detail}`);
    LOG.push(`  [FAIL] ${label} :: ${detail}`);
    console.log(`  [FAIL] ${label} :: ${detail}`);
  }
  return Boolean(cond);
}

async function login(ui, username, password) {
  const res = await fetch(`${ui}/api/system/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username, password }),
  });
  const body = await res.json().catch(() => ({}));
  return (body.data || {}).access_token || null;
}

const REPORT_UI = 'http://127.0.0.1:5195';

/** 每个页面：路由 + 打开详情的选择器 + PDF 按钮 testid + 期望文件名 */
const CASES = [
  { ui: 'http://127.0.0.1:5191', db: 'cloudpivot_pdf03', name: '采购订单', route: '/business/erp/purchase-order',
    open: '[data-testid^="btn-view-"]', btn: 'purchase-detail-pdf', expect: /采购订单-.*\.pdf/, pdfPath: '/purchase-order/' },
  { ui: 'http://127.0.0.1:5191', db: 'cloudpivot_pdf03', name: '销售订单', route: '/business/erp/sales-order',
    open: '[data-testid="btn-so-detail"]', btn: 'so-detail-pdf', expect: /销售订单-.*\.pdf/, pdfPath: '/sales-order/' },
  { ui: 'http://127.0.0.1:5191', db: 'cloudpivot_pdf03', name: '合同', route: '/business/erp/contract',
    open: '[data-testid^="btn-contract-view-"]', btn: 'contract-detail-pdf', expect: /合同-.*\.pdf/, pdfPath: '/contract/' },
  { ui: 'http://127.0.0.1:5191', db: 'cloudpivot_pdf03', name: '收货入库', route: '/business/erp/stock-in',
    open: '[data-testid^="btn-view-"]', btn: 'stockin-detail-pdf', expect: /收货入库单-.*\.pdf/, pdfPath: '/stock-in/' },
  { ui: 'http://127.0.0.1:5191', db: 'cloudpivot_pdf03', name: '出库单', route: '/business/erp/stock-out',
    open: '[data-testid^="btn-view-"]', btn: 'stockout-detail-pdf', expect: /出库单-.*\.pdf/, pdfPath: '/stock-out/' },
  { ui: 'http://127.0.0.1:5191', db: 'cloudpivot_pdf03', name: '调拨单', route: '/business/erp/transfer',
    open: '[data-testid^="btn-view-"]', btn: 'transfer-detail-pdf', expect: /调拨单-.*\.pdf/, pdfPath: '/stock-transfer/' },
  { ui: 'http://127.0.0.1:5191', db: 'cloudpivot_pdf03', name: '盘点单', route: '/business/erp/stock-take',
    open: '[data-testid^="btn-view-"]', btn: 'stocktake-detail-pdf', expect: /盘点单-.*\.pdf/, pdfPath: '/stock-take/' },
  { ui: 'http://127.0.0.1:5192', db: 'cloudpivot_pdf04', name: '采购退货', route: '/business/erp/purchase-return',
    open: '[data-testid="btn-prt-detail"]', btn: 'prt-detail-pdf', expect: /采购退货单-.*\.pdf/, pdfPath: '/purchase-return/' },
  { ui: 'http://127.0.0.1:5192', db: 'cloudpivot_pdf04', name: '销售退货', route: '/business/erp/sales-return',
    open: '[data-testid="btn-return-detail"]', btn: 'return-detail-pdf', expect: /销售退货单-.*\.pdf/, pdfPath: '/sales-return/' },
  { ui: 'http://127.0.0.1:5194', db: 'cloudpivot_pdf06', name: '任务', route: '/task/list',
    open: 'tbody a:has-text("查看")', btn: 'task-detail-pdf', expect: /派工单-.*\.pdf/, pdfPath: '/business/task/' },
  { ui: REPORT_UI, db: 'cloudpivot_pdf07', name: '资产负债表', route: '/business/erp/report/balance-sheet',
    open: null, btn: 'bs-export-pdf', expect: /资产负债表-.*\.pdf/, pdfPath: '/report/balance-sheet/pdf' },
];

/** 只认「后端 PDF 接口」的请求（blob: 里的 pdf.js worker 也带 pdf，别误判） */
function isPdfApiRequest(url, pdfPath) {
  return url.startsWith('http') && url.includes('/api/') && url.includes(pdfPath === '/business/task/' ? '/business/task/' : pdfPath);
}

async function newPage(browser, token, username) {
  const context = await browser.newContext({ acceptDownloads: true, viewport: { width: 1600, height: 1000 } });
  await context.addInitScript(([tk, uname]) => {
    localStorage.setItem('x_access_token', tk);
    // 关掉两个「首次进入」遮罩：项目启动向导 + 新功能引导（否则它们盖住列表，点不到按钮）
    localStorage.setItem(`cloudpivot.setup-wizard.skipped:${uname}`, '1');
    localStorage.setItem('cp-onboarding-done', '1');
  }, [token, username]);
  return { context, page: await context.newPage() };
}

/** 关掉「首次启动向导 / 新功能引导」这类会盖住列表的遮罩（localStorage 标记之外再兜一层） */
async function dismissOverlays(page) {
  for (const text of ['知道了', '稍后再说', '稍后配置', '以后再说']) {
    const btn = page.locator(`button:has-text("${text}")`).first();
    if (await btn.isVisible().catch(() => false)) {
      await btn.click({ force: true }).catch(() => {});
      await page.waitForTimeout(300);
    }
  }
  await page.keyboard.press('Escape').catch(() => {});
  await page.waitForTimeout(200);
}

async function openPage(page, item) {
  await page.goto(`${item.ui}${item.route}`, { waitUntil: 'domcontentloaded', timeout: 60000 });
  await page.waitForLoadState('networkidle', { timeout: 30000 }).catch(() => {});
  await dismissOverlays(page);
}

/** TDesign 的 fixed 列会把操作列的 DOM 复制一份 → 逐个点，直到详情按钮出现 */
async function openDetailUntil(page, selector, btnLocator) {
  const matches = page.locator(selector);
  const total = await matches.count();
  for (let i = 0; i < total; i += 1) {
    if (await btnLocator.isVisible().catch(() => false)) {
      return true;
    }
    await matches.nth(i).click({ force: true }).catch(() => {});
    await page.waitForTimeout(600);
  }
  return btnLocator.isVisible().catch(() => false);
}

/** 报表页：期间没选中时导出按钮是禁用的 → 选一个有数据的期间（2026-10），自动重算 */
async function ensureReportPeriod(page, btn) {
  if (!(await btn.isDisabled().catch(() => false))) return;
  await page.locator('[data-testid="bs-period-select"]').click({ force: true }).catch(() => {});
  await page.waitForTimeout(500);
  const option = page.locator('.t-select-option', { hasText: '2026-10' }).first();
  if (await option.isVisible().catch(() => false)) {
    await option.click({ force: true });
  } else {
    await page.locator('.t-select-option').first().click({ force: true }).catch(() => {});
  }
  await page.waitForTimeout(2500);
}

async function runCase(browser, item, token) {
  const { context, page } = await newPage(browser, token, 'admin');
  const requests = [];
  page.on('request', (req) => {
    if (isPdfApiRequest(req.url(), item.pdfPath)) {
      requests.push({ url: req.url(), auth: req.headers()['authorization'] || '' });
    }
  });
  const errors = [];
  page.on('pageerror', (err) => errors.push(String(err)));

  try {
    await openPage(page, item);
    const btn = page.locator(`[data-testid="${item.btn}"]`);
    if (item.open) {
      await openDetailUntil(page, item.open, btn);
    } else {
      await btn.waitFor({ state: 'visible', timeout: 20000 });
      await ensureReportPeriod(page, btn);
    }
    await btn.waitFor({ state: 'visible', timeout: 20000 });
    check(true, `${item.name}：详情页有「下载 PDF」按钮`);
    check((await btn.innerText()).includes('下载 PDF'), `${item.name}：按钮文案`,
      (await btn.innerText()).trim());
    await page.screenshot({ path: join(OUT, `ui-${item.name}-button.png`) });

    const slow = item.name === '采购订单';
    if (slow) {
      // 慢网络 1.5s：断言点击后按钮进 loading
      await page.route('**/pdf', async (route) => {
        await new Promise((resolve) => setTimeout(resolve, 1500));
        await route.continue();
      });
    }

    const downloadPromise = page.waitForEvent('download', { timeout: 120000 });
    await btn.click({ force: true });
    if (slow) {
      await page.waitForTimeout(500);
      const loadingSeen = await btn
        .evaluate((el) => el.className.includes('loading') || Boolean(el.querySelector('.t-loading'))
          || el.getAttribute('aria-busy') === 'true' || el.disabled)
        .catch(() => false);
      check(loadingSeen === true, '采购订单：下载中按钮进入 loading');
    }
    const download = await downloadPromise;
    if (slow) {
      await page.unroute('**/pdf').catch(() => {});
    }

    const filename = download.suggestedFilename();
    check(item.expect.test(filename), `${item.name}：下载文件名 ${filename}`, filename);
    const saved = join(OUT, `dl-${item.name}.pdf`);
    await download.saveAs(saved);
    const size = statSync(saved).size;
    check(size > 20000, `${item.name}：文件体积 ${size}B > 20KB`, String(size));

    const req = requests[requests.length - 1];
    check(Boolean(req), `${item.name}：发出的是 PDF 接口请求`, JSON.stringify(requests.map((r) => r.url)));
    check(Boolean(req && req.auth.startsWith('Bearer ')), `${item.name}：请求带 Bearer token`,
      req ? `auth=${req.auth.slice(0, 16)}` : 'no request');
    check(errors.length === 0, `${item.name}：页面无 JS 报错`, errors.join(' | ').slice(0, 200));
    await page.screenshot({ path: join(OUT, `ui-${item.name}-after.png`) });
  } catch (err) {
    check(false, `${item.name}：流程异常`, String(err).slice(0, 300));
  } finally {
    await context.close();
  }
}

async function runForbidden(browser, token) {
  const item = CASES[CASES.length - 1]; // 资产负债表
  const { context, page } = await newPage(browser, token, NOPERM_USER);
  try {
    await openPage(page, item);
    const btn = page.locator(`[data-testid="${item.btn}"]`);
    await btn.waitFor({ state: 'visible', timeout: 20000 });
    await ensureReportPeriod(page, btn);
    check(await btn.isVisible(), '无权限用户：报表页按钮可见（没被藏起来）');

    let downloaded = false;
    page.on('download', () => { downloaded = true; });
    await btn.click({ force: true });

    const msg = page.locator('.t-message').first();
    await msg.waitFor({ state: 'visible', timeout: 30000 });
    const text = (await msg.innerText()).trim();
    check(text.includes('权限'), `无权限：提示含「权限」（${text}）`, text);
    check(downloaded === false, '无权限：没有触发下载');
    await page.screenshot({ path: join(OUT, 'ui-403-toast.png') });
  } catch (err) {
    check(false, '无权限：流程异常', String(err).slice(0, 300));
  } finally {
    await context.close();
  }
}

const browser = await chromium.launch({ headless: true });
try {
  const tokens = new Map();
  for (const item of CASES) {
    if (!tokens.has(item.ui)) {
      const token = await login(item.ui, 'admin', ADMIN_PW);
      tokens.set(item.ui, token);
      check(Boolean(token), `${item.db}：管理员登录（${item.ui}）`);
    }
    if (!tokens.get(item.ui)) continue;
    await runCase(browser, item, tokens.get(item.ui));
  }
  const dup = await login(CASES[0].ui, 'pdf08_noperm', 'Pdf08@123456').catch(() => null);
  const rpt = await login(REPORT_UI, NOPERM_USER, NOPERM_PW).catch(() => null);
  void dup;
  if (check(Boolean(rpt), '低权限账号登录（报表可看 / 导出未授权）')) {
    await runForbidden(browser, rpt);
  }
} finally {
  await browser.close();
}

const summary = `\n真实浏览器断言 ${PASS + FAIL} 项：PASS=${PASS} FAIL=${FAIL}`;
console.log(summary);
if (FAILS.length) {
  console.log('失败清单：');
  for (const f of FAILS) console.log(`  - ${f}`);
}
writeFileSync(join(OUT, '../raw_browser.txt'),
  `${LOG.join('\n')}\n${summary}\n${FAILS.map((f) => `FAIL: ${f}`).join('\n')}\n`);
process.exit(FAIL === 0 ? 0 : 1);
