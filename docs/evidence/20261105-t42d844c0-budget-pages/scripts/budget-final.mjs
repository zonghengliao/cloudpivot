/**
 * 预算三页最终验收截图（卡 t_42d844c0，方案 A）。
 *
 * 数据前置（已由真实审批链落库，非直接改库）：
 *   BDG-20261105-000001 财务部/1001 ACTIVE
 *   BDG-20261105-000003 采购部/1002 ACTIVE
 *   BDG-20261105-000004 采购部/1012 ACTIVE
 * 本脚本再走一次「新建 → 提交」真实动作，覆盖 4 个验收动作的截图。
 */
import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';
import fs from 'node:fs';

const BASE = process.env.BASE || 'http://127.0.0.1:5180';
const API = process.env.API || 'http://127.0.0.1:18097';
const OUT = '/root/.hermes/profiles/frontend-developer/cache/scratch/budget-shots';
fs.mkdirSync(OUT, { recursive: true });
for (const f of fs.readdirSync(OUT)) {
  if (/^\d\d-.*\.png$/.test(f)) fs.unlinkSync(`${OUT}/${f}`);
}

const loginBody = await (await fetch(`${API}/api/system/auth/login`, {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ username: 'admin', password: 'Manual@123456' }),
})).json();
const { access_token: token, refresh_token: refresh } = loginBody.data;

const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 }, locale: 'zh-CN' });
await ctx.addInitScript(
  ([a, r]) => {
    localStorage.setItem('x_access_token', a);
    localStorage.setItem('x_refresh_token', r);
    localStorage.setItem('x_guide_done', '1');
    localStorage.setItem('cp-onboarding-done', '1');
    localStorage.setItem('cp-guide-dismissed', '1');
    sessionStorage.setItem('login-unread-shown', '1');
  },
  [token, refresh],
);
const page = await ctx.newPage();
page.setDefaultTimeout(20000);
const errors = [];
page.on('response', (r) => {
  if (r.status() >= 400 && r.url().includes('/api/') && !r.url().includes('/notice/stream')) {
    errors.push(`${r.status()} ${r.url().replace(BASE, '').split('?')[0]}`);
  }
});
page.on('pageerror', (e) => errors.push('pageerror: ' + String(e).split('\n')[0].slice(0, 200)));

let idx = 0;
async function shot(name, opts = {}) {
  idx += 1;
  const file = `${OUT}/${String(idx).padStart(2, '0')}-${name}.png`;
  await page.screenshot({ path: file, fullPage: !!opts.fullPage });
  console.log(`[shot] ${file}`);
}

async function closeUnreadDialog() {
  const btn = page.getByTestId('login-unread-ok');
  if (await btn.count()) {
    await btn.click().catch(() => {});
    await page.waitForTimeout(400);
  }
}

async function gotoRoute(route) {
  await page.goto(`${BASE}${route}`, { waitUntil: 'domcontentloaded' });
  await page.waitForTimeout(1700);
  await closeUnreadDialog();
  const has404 = await page.locator('text=您访问的页面不存在').count();
  if (has404) throw new Error(`${route} 仍是 404`);
  console.log(`[nav] ${route} 404=0 title=${(await page.title()).slice(0, 20)}`);
}

async function pickSelect(testid, index = 0) {
  await page.getByTestId(testid).click();
  await page.waitForTimeout(400);
  const options = page.locator('.t-select-option:visible');
  if (!(await options.count())) throw new Error(`select ${testid} 没有可见选项`);
  const label = (await options.nth(index).innerText()).trim();
  await options.nth(index).click();
  await page.waitForTimeout(250);
  return label;
}

async function dialogConfirm(text) {
  await page.locator('.t-dialog__footer .t-button--theme-primary', { hasText: text }).first().click();
  await page.waitForTimeout(1600);
}

// ==================== ① 列表（3 条 ACTIVE） ====================
await gotoRoute('/business/erp/budget');
const listMeta = (await page.getByTestId('budget-meta').innerText()).trim();
console.log(`[list] ${listMeta}`);
await shot('budget-list');

// ==================== ② 新建预算 ====================
await page.getByTestId('budget-btn-create').click();
await page.waitForTimeout(700);
const deptLabel = await pickSelect('budget-form-dept', 0);
const accountLabel = await pickSelect('budget-form-account', 2);
await page.getByTestId('budget-form-total').locator('input').fill('60000');
await page.waitForTimeout(200);
await page.getByTestId('budget-btn-even').click();
await page.waitForTimeout(400);
console.log(
  `[form] dept=${deptLabel} account=${accountLabel} 合计=${(await page.getByTestId('budget-month-sum').innerText()).trim()} 1月=${await page.getByTestId('budget-month-1').locator('input').inputValue()} 12月=${await page.getByTestId('budget-month-12').locator('input').inputValue()}`,
);
await shot('budget-create');

await dialogConfirm('创建草稿');
await closeUnreadDialog();
const draftNo = await page.evaluate(() => {
  const row = [...document.querySelectorAll('tbody tr')].find(
    (tr) => tr.innerText.includes('草稿') && tr.innerText.includes('BDG-'),
  );
  return (row?.querySelector('[data-testid^="budget-no-"]')?.getAttribute('data-testid') ?? '').replace('budget-no-', '');
});
if (!draftNo) throw new Error('没找到刚建好的草稿行');
console.log(`[create] ${draftNo} status=${(await page.locator(`[data-testid="budget-status-${draftNo}"]`).innerText()).trim()}`);
await shot('budget-list-draft');

// ==================== ③ 上报（提交审批） ====================
await page.locator(`[data-testid="budget-btn-submit-${draftNo}"]`).click();
await page.waitForTimeout(700);
await shot('budget-submit-confirm');
await dialogConfirm('提交审批');
await closeUnreadDialog();
console.log(`[submit] ${draftNo} -> ${(await page.locator(`[data-testid="budget-status-${draftNo}"]`).innerText()).trim()}`);
await shot('budget-list-submitted');

// ==================== ④ 详情抽屉（已生效的那条） ====================
await page.locator('[data-testid="budget-btn-detail-BDG-20261105-000004"]').click();
await page.waitForTimeout(1600);
await shot('budget-detail');
await page.keyboard.press('Escape');
await page.waitForTimeout(600);

// ==================== ⑤ 预算科目映射 ====================
await gotoRoute('/business/erp/budget/mapping');
console.log(`[mapping] ${(await page.getByTestId('budget-mapping-meta').innerText()).trim()}`);
await shot('budget-mapping-list');
await page.getByTestId('budget-mapping-btn-create').click();
await page.waitForTimeout(700);
await pickSelect('budget-mapping-form-account', 1);
await shot('budget-mapping-dialog');
await page.locator('.t-dialog__footer .t-button:not(.t-button--theme-primary)').first().click();
await page.waitForTimeout(600);

// ==================== ⑥ 预算执行报表 ====================
await gotoRoute('/business/erp/budget/report');
console.log(`[report] ${(await page.getByTestId('budget-report-meta').innerText()).trim()}`);
await shot('budget-report');
await page.getByTestId('budget-report-toggle-all').click();
await page.waitForTimeout(900);
await shot('budget-report-expanded', { fullPage: true });

console.log('[api-errors]', errors.length ? errors : 'none');
await browser.close();
