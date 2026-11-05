/**
 * P1-FIX-9 真机 e2e：盘点 + 期初（隔离实例 15190 前端 / 18190 后端 / cloudpivot_st9）
 * 产出截图到 /root/st9-run/shots/，每步日志到 /root/st9-run/e2e.txt
 */
const { chromium } = require('/opt/cloudpivot/frontend/node_modules/@playwright/test');
const fs = require('fs');

const BASE = 'http://127.0.0.1:15190';
const API = 'http://127.0.0.1:18190/api';
const SHOTS = '/root/st9-run/shots';
const LOG = [];
const RESULTS = [];

fs.mkdirSync(SHOTS, { recursive: true });

function log(title, payload) {
  const text = typeof payload === 'string' ? payload : JSON.stringify(payload, null, 2);
  const block = `\n### ${title}\n${text}`;
  console.log(block);
  LOG.push(block);
}

function check(name, ok, detail) {
  RESULTS.push({ name, pass: !!ok });
  log(`[${ok ? 'PASS' : 'FAIL'}] ${name}`, detail || '');
}

async function shot(page, name) {
  const p = `${SHOTS}/${name}.png`;
  await page.screenshot({ path: p, fullPage: true });
  log(`截图 ${name}`, p);
  return p;
}

async function login(page, user, pass) {
  await page.goto(`${BASE}/login`, { waitUntil: 'domcontentloaded' });
  await page.getByPlaceholder('请输入用户名').fill(user);
  await page.getByPlaceholder('请输入密码').fill(pass);
  await page.getByRole('button', { name: /登\s*录/ }).click();
  await page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 30000 });
  await page.waitForTimeout(1200);
}

async function pickSelect(page, testid, optionText) {
  await page.locator(`[data-testid="${testid}"]`).click();
  const dd = page.locator('.t-select__dropdown:visible').last();
  await dd.waitFor({ state: 'visible', timeout: 10000 });
  const opt = dd.locator('.t-select-option', { hasText: optionText }).first();
  await opt.click();
  await page.waitForTimeout(300);
}

async function apiApprove(taskHintTakeNo) {
  // 审批走引擎接口（同一实例、真库）；页面侧只验收盘点模块本身
  const login = await fetch(`${API}/system/auth/login`, {
    method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username: 'st_wh', password: '${ST9_WH_PASSWORD}' }),
  }).then((r) => r.json());
  const token = login.data.access_token;
  const todo = await fetch(`${API}/business/flow/task/todo?page=1&page_size=20`, { headers: { Authorization: 'Bearer ' + token } }).then((r) => r.json());
  const card = (todo.data.list || []).find((t) => JSON.stringify(t).includes(taskHintTakeNo));
  if (!card) throw new Error('待办里找不到 ' + taskHintTakeNo + '：' + JSON.stringify(todo.data).slice(0, 300));
  const res = await fetch(`${API}/business/flow/task/${card.id}/approve`, {
    method: 'POST', headers: { 'Content-Type': 'application/json', Authorization: 'Bearer ' + token },
    body: JSON.stringify({ comment: 'P1-FIX-9 真机验收：同意' }),
  }).then((r) => r.json());
  return { task_id: card.id, resp: res };
}

(async () => {
  const browser = await chromium.launch({ headless: true });
  const ctx = await browser.newContext({ viewport: { width: 1600, height: 1000 } });
  // E10 新功能引导浮层会拦点击（skill 已记录）：写死 localStorage 跳过
  await ctx.addInitScript(() => {
    try { window.localStorage.setItem('cp-onboarding-done', '1'); } catch (e) { /* ignore */ }
  });
  const page = await ctx.newPage();
  page.setDefaultTimeout(45000);

  try {
    // ---------- 登录 ----------
    await login(page, 'admin', '${ST9_ADMIN_PASSWORD}');
    check('admin 真机登录成功', page.url().includes('/'), page.url());

    // ---------- ① 期初页面 ----------
    await page.goto(`${BASE}/business/erp/opening`, { waitUntil: 'domcontentloaded' });
    await page.locator('[data-testid="opening-state-tag"]').waitFor({ timeout: 45000 });
    await page.waitForTimeout(1500);
    // 页面默认期间不是 2026-11（默认取的是「下一期末尾」），显式切到验收入的期间
    const curPeriod = await page.locator('[data-testid="opening-period-select"]').innerText().catch(() => '');
    log('期初页默认期间', curPeriod);
    if (!curPeriod.includes('2026-11')) {
      await pickSelect(page, 'opening-period-select', '2026-11');
      await page.waitForTimeout(2500);
    }
    const periodText = await page.locator('[data-testid="opening-period-select"]').innerText().catch(() => '');
    const stateText = await page.locator('[data-testid="opening-state-tag"]').innerText();
    const debitTotal = await page.locator('[data-testid="opening-total-debit"]').innerText().catch(() => '');
    const creditTotal = await page.locator('[data-testid="opening-total-credit"]').innerText().catch(() => '');
    const voucherInfo = await page.locator('[data-testid="opening-voucher-info"]').innerText().catch(() => '');
    log('期初页（按科目录入）', { stateText, debitTotal, creditTotal, voucherInfo });
    await shot(page, '01-opening-account-tab');
    check('期初页显示已录状态 + 凭证信息', /已录|ENTERED/.test(stateText) && voucherInfo.length > 0, { stateText, voucherInfo });

    // 库存期初 tab
    await page.locator('[data-testid="opening-tabs"] .t-tabs__nav-item', { hasText: '库存期初' }).first().click();
    await page.waitForTimeout(1200);
    const invTotal = await page.locator('[data-testid="opening-inventory-total"]').innerText().catch(() => '');
    const invQty = await page.locator('[data-testid="opening-inventory-qty-0"] input').inputValue().catch(() => '');
    log('期初页（库存期初）', { invTotal, invQty });
    await shot(page, '02-opening-inventory-tab');
    check('库存期初页显示 100 / 1,000.00', invQty.startsWith('100') && /1,?000/.test(invTotal), { invQty, invTotal });

    // 资产负债表（期初建账结果）
    await page.goto(`${BASE}/business/erp/report/balance-sheet`, { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(4000);
    await shot(page, '03-balance-sheet-after-opening');
    const bodyText = await page.locator('body').innerText();
    check('资产负债表含存货 1,000.00 / 资产总计 6,000.00',
      /1,000\.00/.test(bodyText) && /6,000\.00/.test(bodyText) && /存货/.test(bodyText),
      { hasInventory: /存货/.test(bodyText), has1000: /1,000\.00/.test(bodyText) });

    // ---------- ② 盘点页面 ----------
    await page.goto(`${BASE}/business/erp/stock-take`, { waitUntil: 'domcontentloaded' });
    await page.locator('[data-testid="stocktake-table"]').waitFor({ timeout: 45000 });
    await page.waitForTimeout(1200);
    await shot(page, '04-stocktake-list');
    const meta = await page.locator('[data-testid="stocktake-meta"]').innerText();
    log('盘点列表', meta);

    // 新建（UI 建单）
    await page.locator('[data-testid="btn-stocktake-create"]').click();
    await page.locator('[data-testid="stocktake-form-drawer"]').waitFor({ timeout: 20000 });
    await pickSelect(page, 'stocktake-form-warehouse', 'P1-FIX-9 验收总仓');
    await page.locator('[data-testid="stocktake-form-remark"] textarea').fill('P1-FIX-9 真机验收：UI 建单');
    await page.waitForTimeout(400);
    await shot(page, '05-stocktake-create-drawer');
    await page.locator('[data-testid="btn-stocktake-save"]').click();
    // 保存成功后页面直接打开「盘点单详情」抽屉（不再回到列表），以详情单号为准
    await page.locator('[data-testid="stocktake-detail-no"]').waitFor({ timeout: 30000 });
    await page.waitForTimeout(1500);
    const takeNo = (await page.locator('[data-testid="stocktake-detail-no"]').innerText()).trim();
    const listRows = await page.locator('[data-testid^="stocktake-no-"]').allInnerTexts().catch(() => []);
    log('UI 建单结果', { detailTakeNo: takeNo, listRows });
    check('UI 建单成功（详情抽屉出现新单号 ST-）', /^ST-\d{8}-\d{6}$/.test(takeNo), { takeNo });

    // 详情抽屉已自动打开（保存后），直接读账面数
    const bookQty = await page.locator('[data-testid^="stocktake-book-"]').first().innerText();
    await shot(page, '06-stocktake-detail-draft');
    log('详情（草稿）', { takeNo, bookQty });
    check('详情账面数 = 100（服务端快照）', /100/.test(bookQty), { bookQty });

    // 录实盘 90（盘亏 10）
    const actualInput = page.locator('[data-testid="stocktake-actual-input-1"] input');
    await actualInput.fill('90');
    await page.locator('[data-testid="btn-stocktake-save-actuals"]').click();
    await page.waitForTimeout(2000);
    const diffText = await page.locator('[data-testid="stocktake-detail-diff"]').innerText();
    const diffQty = await page.locator('[data-testid="stocktake-diff-qty-1"]').innerText().catch(() => '');
    await shot(page, '07-stocktake-actual-diff');
    log('录入实盘 90 后', { diffText, diffQty });
    check('差异行显示 -10', /-10/.test(diffQty), { diffQty });

    // 提交审批
    await page.locator('[data-testid="btn-stocktake-submit"]').click();
    await page.locator('[data-testid="btn-stocktake-submit-confirm"]').waitFor({ timeout: 15000 });
    await page.locator('[data-testid="btn-stocktake-submit-confirm"]').click();
    await page.waitForTimeout(3000);
    const statusAfterSubmit = await page.locator('[data-testid="stocktake-detail-status"]').innerText();
    await shot(page, '08-stocktake-pending-post');
    log('提交审批后', { statusAfterSubmit });
    check('提交后状态 = 待过账', /待过账/.test(statusAfterSubmit), { statusAfterSubmit });

    // 审批（st_wh，引擎接口）
    const apr = await apiApprove(takeNo);
    log('仓管主管审批（引擎接口）', apr);
    check('审批接口返回成功', apr.resp && (apr.resp.code === 0 || apr.resp.code === 200 || apr.resp.code === undefined),
      apr.resp);

    // 过账
    await page.reload({ waitUntil: 'domcontentloaded' });
    await page.locator('[data-testid="stocktake-table"]').waitFor({ timeout: 30000 });
    await page.locator(`[data-testid="btn-view-${takeNo}"]`).click();
    await page.locator('[data-testid="btn-stocktake-post"]').waitFor({ timeout: 20000 });
    await page.locator('[data-testid="btn-stocktake-post"]').click();
    await page.locator('[data-testid="btn-stocktake-post-confirm"]').waitFor({ timeout: 15000 });
    await page.locator('[data-testid="btn-stocktake-post-confirm"]').click();
    await page.waitForTimeout(3500);
    const statusAfterPost = await page.locator('[data-testid="stocktake-detail-status"]').innerText();
    const voucherText = await page.locator('[data-testid="stocktake-detail-voucher"]').innerText();
    await shot(page, '09-stocktake-posted');
    log('过账后详情', { statusAfterPost, voucherText });
    check('过账后状态 = 已过账 且差异凭证已回写', /已过账/.test(statusAfterPost) && !/^—/.test(voucherText.trim()),
      { statusAfterPost, voucherText });

    // 库存页（库存已调整）
    await page.goto(`${BASE}/business/erp/inventory`, { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(4000);
    const invBody = await page.locator('body').innerText();
    await shot(page, '10-inventory-after-post');
    check('库存页可见 ST9-M-001 且数量 90', /ST9-M-001/.test(invBody) && /90\.00|90\b/.test(invBody), {});

    // 作废回滚
    await page.goto(`${BASE}/business/erp/stock-take`, { waitUntil: 'domcontentloaded' });
    await page.locator('[data-testid="stocktake-table"]').waitFor({ timeout: 30000 });
    await page.locator(`[data-testid="btn-view-${takeNo}"]`).click();
    await page.locator('[data-testid="btn-stocktake-cancel"]').waitFor({ timeout: 20000 });
    await page.locator('[data-testid="btn-stocktake-cancel"]').click();
    await page.locator('[data-testid="stocktake-cancel-reason"] textarea').fill('P1-FIX-9 真机验收：作废回滚验证');
    await page.locator('[data-testid="btn-stocktake-cancel-confirm"]').click();
    await page.waitForTimeout(3500);
    const statusAfterCancel = await page.locator('[data-testid="stocktake-detail-status"]').innerText();
    await shot(page, '11-stocktake-cancelled');
    log('作废后状态', { statusAfterCancel });
    check('作废后状态 = 已作废', /已作废/.test(statusAfterCancel), { statusAfterCancel });

    await page.goto(`${BASE}/business/erp/inventory`, { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(4000);
    const invBody2 = await page.locator('body').innerText();
    await shot(page, '12-inventory-after-cancel');
    check('作废回滚后库存页回到 100', /ST9-M-001/.test(invBody2) && /100\.00|100\b/.test(invBody2), {});

    log('===== e2e 结论 =====', { total: RESULTS.length, fails: RESULTS.filter((r) => !r.pass).map((r) => r.name) });
  } catch (err) {
    log('E2E 异常', String(err && err.stack || err).slice(0, 1500));
    try { await shot(page, 'zz-e2e-failure'); } catch (e) { /* ignore */ }
    RESULTS.push({ name: 'e2e-执行未中断', pass: false });
  } finally {
    fs.writeFileSync('/root/st9-run/e2e.txt', LOG.join('\n') + '\n\n## VERDICT\n' + JSON.stringify(RESULTS, null, 2) + '\n');
    await browser.close();
  }
  const fails = RESULTS.filter((r) => !r.pass);
  console.log('FAILS:', JSON.stringify(fails.map((r) => r.name)));
  process.exit(fails.length ? 1 : 0);
})();
