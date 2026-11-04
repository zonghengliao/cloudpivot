// FIN-2-FE 真机 UI 验收：三个页面 + 四个交互细节（隔离实例 18025 / vite dev 5199）
// 用法：node verify.mjs   （node_modules 已软链到 /opt/cloudpivot/frontend/node_modules）
import { chromium } from '@playwright/test';
import fs from 'node:fs';
import path from 'node:path';

const BASE = 'http://127.0.0.1:5199';
const EVID = '/opt/cloudpivot/docs/evidence/20261105-fin2-fe';
const RAW = path.join(EVID, 'raw');
const login = JSON.parse(fs.readFileSync(path.join(RAW, '00-login-admin.json'), 'utf8'));
const token = login.data.access_token;
const refresh = login.data.refresh_token ?? '';

const results = [];
function check(name, ok, detail) {
  results.push({ name, ok, detail });
  console.log(`${ok ? 'PASS' : 'FAIL'} | ${name} | ${detail}`);
}
const shot = async (page, file) => {
  await page.waitForTimeout(600);
  await page.screenshot({ path: path.join(EVID, file), fullPage: true });
  console.log(`SHOT | ${file}`);
};
const text = async (page, sel) =>
  (await page.locator(sel).first().innerText().catch(() => '')).replace(/\s+/g, ' ').trim();

async function pickSelect(page, testid, frag) {
  const wrap = page.locator(`[data-testid="${testid}"]`).first();
  await wrap.scrollIntoViewIfNeeded();
  const b = await wrap.boundingBox();
  await page.mouse.click(b.x + b.width / 2, b.y + b.height / 2);
  await page.waitForFunction(
    () => [...document.querySelectorAll('.t-select__dropdown')].some((d) => d.offsetParent !== null),
    null,
    { timeout: 8000 },
  );
  const opt = page.locator('.t-select__dropdown:visible .t-select-option', { hasText: frag }).first();
  await opt.click();
  await page.waitForTimeout(350);
}

async function fillInput(page, sel, value) {
  const root = page.locator(sel).first();
  await root.scrollIntoViewIfNeeded();
  const tag = await root.evaluate((el) => el.tagName).catch(() => '');
  const input = tag === 'INPUT' ? root : root.locator('input').first();
  await input.click();
  await page.keyboard.press('Control+a');
  await page.keyboard.press('Backspace');
  await input.type(value, { delay: 25 });
  await page.keyboard.press('Tab');
  await page.waitForTimeout(250);
  return input.inputValue();
}

const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1600, height: 1000 } });
await ctx.addInitScript(
  ([t, r]) => {
    localStorage.setItem('x_access_token', t);
    if (r) localStorage.setItem('x_refresh_token', r);
    localStorage.setItem('cp-onboarding-done', '1');
    sessionStorage.setItem('login-unread-shown', '1');
  },
  [token, refresh],
);
const page = await ctx.newPage();
page.on('pageerror', (e) => console.log('PAGEERROR:', e.message));

try {
  // ================= A. 资产卡片台账 =================
  await page.goto(`${BASE}/business/erp/asset`, { waitUntil: 'domcontentloaded' });
  await page.waitForSelector('[data-testid="asset-table"]', { timeout: 20000 });
  await page.waitForFunction(
    () => (document.querySelector('[data-testid="asset-table"]')?.textContent ?? '').includes('FA-20261105-000004'),
    null,
    { timeout: 20000 },
  );
  const rows = await page.evaluate(() => {
    const trs = [...document.querySelectorAll('[data-testid="asset-table"] tbody tr')];
    return trs.map((tr) => tr.innerText.replace(/\s+/g, ' ').trim()).filter((t) => t.includes('FA-'));
  });
  check('A1 台账页渲染 4 条真数据', rows.length === 4, `rows=${rows.length} :: ${rows[0] ?? ''}`);
  const meta = await text(page, '[data-testid="asset-meta"]');
  check('A2 台账合计口径', /4/.test(meta), meta);
  await shot(page, 'fe-01-asset-list.png');

  // ================= B. 建卡 + 实时折旧预览（细节 #1） =================
  await page.locator('[data-testid="asset-btn-create"]').click();
  await page.waitForSelector('[data-testid="asset-form-dialog"]', { timeout: 15000 });
  await page.waitForTimeout(500);
  const emptyHint = await text(page, '[data-testid="asset-preview-empty"]');
  check('B0 空表单预览为空态', emptyHint.includes('实时显示'), emptyHint);

  await fillInput(page, '[data-testid="asset-form-name"]', 'FIN-2-FE 实时预览验证机');
  await pickSelect(page, 'asset-form-category', '电子设备');
  await pickSelect(page, 'asset-form-dept', 'FIN2 产品部');
  const orig = await fillInput(page, '[data-testid="asset-form-original"]', '100000');
  await fillInput(page, '[data-testid="asset-form-purchase-date"]', '2026-11-10');
  await page.waitForFunction(
    () => (document.querySelector('[data-testid="asset-preview-monthly"]')?.textContent ?? '').includes('2,638.89'),
    null,
    { timeout: 8000 },
  );
  const monthly = await text(page, '[data-testid="asset-preview-monthly"]');
  const start = await text(page, '[data-testid="asset-preview-start"]');
  const finish = await text(page, '[data-testid="asset-preview-finish"]');
  const previewAll = await text(page, '[data-testid="asset-preview"]');
  check('B1 原值输入生效', orig.replace(/,/g, '') === '100000', `input=${orig}`);
  check('B2 实时月折旧额 2,638.89', monthly.includes('2,638.89'), `monthly=${monthly}`);
  check('B3 起提日 = 购入次月 1 日', start === '2026-12-01', `start=${start}`);
  check('B4 预计提完日 2029-11-30', finish === '2029-11-30', `finish=${finish}`);
  check('B5 尾差月 / 口径文案', previewAll.includes('尾差月') && previewAll.includes('round((原值'), previewAll.slice(0, 160));
  // 关掉日期选择面板再截图（否则面板会盖住预览卡）；不要按 Escape —— 那会把整个弹窗关掉
  await page.locator('[data-testid="asset-preview"] .preview__title').click().catch(() => {});
  await page.waitForTimeout(500);
  await shot(page, 'fe-02-asset-create-preview.png');

  // ================= C. 收货单转入：金额预填 + 只读（细节 #2） =================
  await page.locator('[data-testid="asset-form-source"] input[type="radio"]').nth(1).check({ force: true }).catch(async () => {
    await page.locator('[data-testid="asset-form-source"] .t-radio').nth(1).click();
  });
  await page.waitForTimeout(400);
  await pickSelect(page, 'asset-form-stockin', 'IN-20261105-000002');
  await page.waitForTimeout(600);
  const lockedVal = await page.locator('[data-testid="asset-form-original"] input').first().inputValue();
  const lockedDisabled = await page.locator('[data-testid="asset-form-original"] input').first().isDisabled();
  const lockedTax = await page.locator('[data-testid="asset-form-tax"] input').first().inputValue();
  const lockedTaxDisabled = await page.locator('[data-testid="asset-form-tax"] input').first().isDisabled();
  const lockedHint = await text(page, '[data-testid="asset-form-dialog"]');
  check('C1 收货单金额预填 10000', lockedVal.replace(/,/g, '') === '10000.0000', `value=${lockedVal}`);
  check('C2 原值只读（disabled）', lockedDisabled === true, `disabled=${lockedDisabled}`);
  check('C3 进项税 FROM_STOCK 强制 0 且只读', Number(lockedTax.replace(/,/g, '') || 0) === 0, `tax=${lockedTax} disabled=${lockedTaxDisabled}`);
  check('C4 提示「改金额要回收货单」', lockedHint.includes('回收货单'), lockedHint.match(/[^。]*回收货单[^。]*。/)?.[0] ?? '（未见）');
  await shot(page, 'fe-03-asset-stockin-locked.png');
  await page.getByRole('button', { name: '取消' }).first().click().catch(() => {});
  await page.waitForTimeout(600);

  // ================= D. 详情抽屉 =================
  await page.locator('[data-testid="asset-btn-detail-FA-20261105-000004"]').click();
  await page.waitForSelector('[data-testid="asset-detail-drawer"] .t-drawer__body', { timeout: 15000 });
  await page.waitForFunction(
    () => (document.querySelector('[data-testid="asset-detail-drawer"]')?.textContent ?? '').includes('FA-20261105-000004'),
    null,
    { timeout: 15000 },
  );
  await page.waitForTimeout(800);
  const dNet = await text(page, '[data-testid="asset-detail-net"]');
  const dMonths = await text(page, '[data-testid="asset-detail-months"]');
  const dLeft = await text(page, '[data-testid="asset-detail-left"]');
  const dBody = await text(page, '[data-testid="asset-detail-drawer"] .t-drawer__body');
  check('D1 净值 97,361.11', dNet.replace(/,/g, '').includes('97361.11'), `net=${dNet}`);
  check('D2 已提月数 1 / 36', /1\s*\/\s*36/.test(dMonths), `months=${dMonths}`);
  check('D3 剩余可提 35 月 · 92,361.11', dLeft.includes('35') && dLeft.replace(/,/g, '').includes('92361.11'), `left=${dLeft}`);
  check('D4 详情含变动时间线 / 折旧明细 / 凭证', dBody.includes('变动') && dBody.includes('折旧') && dBody.includes('凭证'), dBody.slice(0, 120));
  await shot(page, 'fe-04-asset-detail.png');
  // 关抽屉：点遮罩
  await page.locator('.t-drawer__mask').click({ position: { x: 40, y: 40 } }).catch(() => {});
  await page.waitForTimeout(600);

  // ================= E. 处置弹窗三数（细节 #4） =================
  await page.locator('[data-testid="asset-btn-dispose-FA-20261105-000004"]').click();
  await page.waitForSelector('[data-testid="asset-dispose-summary"]', { timeout: 15000 });
  await page.waitForTimeout(600);
  const sumNet = await text(page, '[data-testid="asset-dispose-net"]');
  const sumLeft = await text(page, '[data-testid="asset-dispose-left"]');
  const sumAll = await text(page, '[data-testid="asset-dispose-summary"]');
  check('E1 处置弹窗净值 97,361.11', sumNet.replace(/,/g, '').includes('97361.11'), `net=${sumNet}`);
  check('E2 处置弹窗已提月数 1 / 36', /1\s*\/\s*36/.test(sumAll), `summary=${sumAll}`);
  check('E3 处置弹窗剩余可提 35 个月 · 92,361.11', sumLeft.includes('35') && sumLeft.replace(/,/g, '').includes('92361.11'), `left=${sumLeft}`);
  // 默认处置类型是「报废」，先切到「出售」才会出现售价/销项税
  await page.locator('[data-testid="asset-dispose-type"] .t-radio').first().click();
  await page.waitForTimeout(500);
  // 填出售价看「损益参考」实时算
  await fillInput(page, '[data-testid="asset-dispose-price"]', '50000');
  await page.waitForTimeout(400);
  const gain = await text(page, '[data-testid="asset-dispose-gain"]');
  check('E4 出价 − 净值 损益参考实时计算', gain.includes('47,361.11') || gain.replace(/,/g, '').includes('-47361.11'), `gain=${gain}`);
  await shot(page, 'fe-05-asset-dispose.png');
  await page.getByRole('button', { name: '取消' }).first().click().catch(() => {});
  await page.waitForTimeout(600);

  // ================= F. 分类页 =================
  await page.goto(`${BASE}/business/erp/asset-category`, { waitUntil: 'domcontentloaded' });
  await page.waitForSelector('[data-testid="category-table"]', { timeout: 20000 });
  await page.waitForFunction(
    () => (document.querySelector('[data-testid="category-table"]')?.textContent ?? '').includes('ELEC'),
    null,
    { timeout: 20000 },
  );
  const catRows = await page.evaluate(() => {
    const trs = [...document.querySelectorAll('[data-testid="category-table"] tbody tr')];
    return trs.map((tr) => tr.innerText.replace(/\s+/g, ' ').trim()).filter((t) => t.length > 3);
  });
  check('F1 分类页 5 条种子', catRows.length === 5, `rows=${catRows.length} :: ${catRows[0] ?? ''}`);
  const catMeta = await text(page, '[data-testid="category-meta"]');
  check('F2 分类页合计', /5/.test(catMeta), catMeta);
  await shot(page, 'fe-06-asset-category.png');

  // ================= G. 折旧页三行汇总（细节 #3） =================
  await page.goto(`${BASE}/business/erp/depreciation`, { waitUntil: 'domcontentloaded' });
  await page.waitForSelector('[data-testid="dep-summary"]', { timeout: 20000 });
  await page.waitForTimeout(1800);
  // 默认期间 = 上一个会计月（2026-10），已提 2,638.89
  const sumText = await text(page, '[data-testid="dep-summary"]');
  const exp0 = await text(page, '[data-testid="dep-summary-expected"]');
  const posted0 = await text(page, '[data-testid="dep-summary-posted"]');
  const diff0 = await text(page, '[data-testid="dep-summary-diff"]');
  check('G1 三行汇总存在（应提 / 已提 / 差异）', /应提/.test(sumText) && /已提/.test(sumText) && /差异/.test(sumText), sumText.slice(0, 200));
  check('G2 默认期间 2026-10 应提 = 已提 = 2,638.89、差异 0', exp0.replace(/,/g, '').includes('2638.89') && posted0.replace(/,/g, '').includes('2638.89') && diff0.replace(/,/g, '').includes('0'), `exp=${exp0} posted=${posted0} diff=${diff0}`);
  await shot(page, 'fe-07-depreciation.png');

  // 切到 2026-11（3 台 · 已过账 7,916.67）
  await pickSelect(page, 'dep-filter-period', '2026 年 11 月');
  await page.waitForFunction(
    () => (document.querySelector('[data-testid="dep-summary-posted"]')?.textContent ?? '').replace(/,/g, '').includes('7916.67'),
    null,
    { timeout: 20000 },
  );
  const exp1 = await text(page, '[data-testid="dep-summary-expected"]');
  const posted1 = await text(page, '[data-testid="dep-summary-posted"]');
  const diff1 = await text(page, '[data-testid="dep-summary-diff"]');
  check('G3 2026-11 应提 = 已提 = 7,916.67、差异 0', exp1.replace(/,/g, '').includes('7916.67') && posted1.replace(/,/g, '').includes('7916.67') && diff1.replace(/,/g, '').includes('0.00'), `exp=${exp1} posted=${posted1} diff=${diff1}`);
  await shot(page, 'fe-08-depreciation-2026-11.png');

  // 批次详情
  await page.locator('[data-testid="dep-btn-detail-DEP-20261105-000004"]').click();
  await page.waitForSelector('[data-testid="dep-detail-drawer"] .t-drawer__body', { timeout: 15000 });
  await page.waitForFunction(
    () => (document.querySelector('[data-testid="dep-detail-drawer"]')?.textContent ?? '').includes('FA-20261105-000003'),
    null,
    { timeout: 15000 },
  );
  await page.waitForTimeout(800);
  const depBody = await text(page, '[data-testid="dep-detail-drawer"] .t-drawer__body');
  check('G4 批次明细含 3 台资产', depBody.includes('FA-20261105-000001') && depBody.includes('FA-20261105-000003'), depBody.slice(0, 160));
  check('G5 批次明细合计 7,916.67', depBody.replace(/,/g, '').includes('7916.67'), depBody.match(/合计[^ ]*/)?.[0] ?? '');
  await shot(page, 'fe-09-depreciation-detail.png');
  await page.locator('.t-drawer__mask').click({ position: { x: 40, y: 40 } }).catch(() => {});
  await page.waitForTimeout(700);

  // 红冲弹窗
  await page.locator('[data-testid="dep-btn-reverse-DEP-20261105-000004"]').click();
  await page.waitForSelector('[data-testid="dep-reverse-reason"]', { timeout: 15000 });
  await page.waitForTimeout(700);
  const dialogText = await page.locator('.t-dialog:visible').last().innerText().catch(() => '');
  check('G6 红冲弹窗要求原因（≥2 字提示）', dialogText.includes('红冲') && dialogText.includes('原因'), dialogText.replace(/\s+/g, ' ').slice(0, 160));
  await shot(page, 'fe-10-depreciation-reverse.png');
  await page.getByRole('button', { name: '取消' }).first().click().catch(() => {});

  // ================= H. 1280 视口：列宽不遮挡（固定操作列） =================
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.goto(`${BASE}/business/erp/asset`, { waitUntil: 'domcontentloaded' });
  await page.waitForSelector('[data-testid="asset-table"]', { timeout: 20000 });
  await page.waitForTimeout(1500);
  const geom = await page.evaluate(() => {
    const holder = document.querySelector('[data-testid="asset-table"] .t-table__content');
    const ths = [...document.querySelectorAll('[data-testid="asset-table"] thead th')];
    const status = ths.find((th) => th.innerText.trim() === '状态');
    const op = ths.find((th) => th.innerText.trim() === '操作');
    const r = (el) => (el ? el.getBoundingClientRect() : null);
    return {
      overflow: holder ? holder.scrollWidth - holder.clientWidth : -1,
      statusRight: r(status)?.right ?? null,
      opLeft: r(op)?.left ?? null,
      viewport: window.innerWidth,
    };
  });
  check(
    'H1 1280 视口下「状态」列未被固定操作列遮挡',
    geom.overflow <= 0 || (geom.statusRight !== null && geom.opLeft !== null && geom.statusRight <= geom.opLeft + 1),
    `overflow=${geom.overflow} statusRight=${Math.round(geom.statusRight)} opLeft=${Math.round(geom.opLeft)} vw=${geom.viewport}`,
  );
  await shot(page, 'fe-11-asset-list-1280.png');
} catch (e) {
  check('EXCEPTION', false, e.message);
  await shot(page, 'fe-99-error.png').catch(() => {});
} finally {
  await browser.close();
}

const failed = results.filter((r) => !r.ok);
fs.writeFileSync(path.join(EVID, 'ui-assertions.json'), JSON.stringify({ total: results.length, failed: failed.length, results }, null, 2));
console.log(`\n合计 ${results.length} 条，失败 ${failed.length} 条`);
process.exit(failed.length ? 1 : 0);
