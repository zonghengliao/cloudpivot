/**
 * t_3a641ac8 真机验收：F2 盘点详情凭证号 + F3 期初页默认期间
 *
 * 环境：前端 vite dev http://127.0.0.1:15191 → 后端 http://127.0.0.1:18191
 *       库 cloudpivot_fix9b（cloudpivot_st9 的克隆，隔离实例，不碰生产）
 * 用法：node fix9b_e2e.cjs round1|round2
 */
const { chromium } = require('/opt/cloudpivot/frontend/node_modules/@playwright/test');
const fs = require('fs');

const ROUND = process.argv[2] || 'round1';
const BASE = 'http://127.0.0.1:15191';
const API = 'http://127.0.0.1:18191/api';
const OUT = process.env.FIX9B_SHOTS || `/root/fix9b-run/shots/${ROUND}`;
const LOG_PATH = process.env.FIX9B_LOG_DIR ? `${process.env.FIX9B_LOG_DIR}/${ROUND}.txt` : `/root/fix9b-run/${ROUND}.txt`;
const USER = 'admin';
const PASS = process.env.FIX9B_PASSWORD;
if (!PASS) {
  console.error('需要 FIX9B_PASSWORD（隔离库 admin 口令，勿写进仓库）');
  process.exit(2);
}

fs.mkdirSync(OUT, { recursive: true });

const LINES = [];
const RESULTS = [];
function log(title, payload) {
  const text = typeof payload === 'string' ? payload : JSON.stringify(payload, null, 2);
  const block = `\n### ${title}\n${text}`;
  console.log(block);
  LINES.push(block);
}
function check(name, ok, detail) {
  RESULTS.push({ name, pass: !!ok });
  log(`[${ok ? 'PASS' : 'FAIL'}] ${name}`, detail === undefined ? '' : detail);
}
async function shot(page, name) {
  const p = `${OUT}/${name}.png`;
  await page.screenshot({ path: p, fullPage: true });
  log(`截图 ${name}`, p);
  return p;
}
/** TDesign select 当前显示的文本 */
async function selectText(page, testid) {
  const box = page.locator(`[data-testid="${testid}"]`);
  const inputVal = await box.locator('input').first().inputValue().catch(() => '');
  const txt = (await box.innerText().catch(() => '')).trim() || inputVal;
  return { txt, inputVal };
}
async function pickPeriod(page, code) {
  await page.locator('[data-testid="opening-period-select"]').click();
  const dd = page.locator('.t-select__dropdown:visible').last();
  await dd.waitFor({ state: 'visible', timeout: 10000 });
  await dd.locator('.t-select-option', { hasText: code }).first().click();
  await page.waitForTimeout(2500);
}

async function hideOnboarding(page) {
  // 首访引导气泡（.onboarding）会盖住表格里的「详情」链接，拦截点击；截图也会挡住内容
  await page
    .addStyleTag({ content: '.onboarding,[data-onboarding-hint]{display:none!important;pointer-events:none!important}' })
    .catch(() => {});
  await page.keyboard.press('Escape').catch(() => {});
}

(async () => {
  const browser = await chromium.launch();
  const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
  page.on('console', (m) => {
    if (m.type() === 'error') LINES.push('[browser-error] ' + m.text());
  });
  try {
    // ---------- 登录 ----------
    await page.goto(`${BASE}/login`, { waitUntil: 'domcontentloaded' });
    await page.getByPlaceholder('请输入用户名').fill(USER);
    await page.getByPlaceholder('请输入密码').fill(PASS);
    await page.getByRole('button', { name: /登\s*录/ }).click();
    await page.waitForURL((u) => !u.pathname.startsWith('/login'), { timeout: 30000 });
    await page.waitForTimeout(1200);
    log('登录', { ok: true, url: page.url() });

    // ================================================== F3 期初页默认期间
    await page.goto(`${BASE}/business/erp/opening`, { waitUntil: 'domcontentloaded' });
    await page.locator('[data-testid="opening-period-select"]').waitFor({ timeout: 45000 });
    await hideOnboarding(page);
    await page.waitForTimeout(3000);
    const picked = await selectText(page, 'opening-period-select');
    const openingTotalDebit = await page.locator('[data-testid="opening-total-debit"]').innerText().catch(() => '');
    const stateTag = await page.locator('[data-testid="opening-period-status"]').innerText().catch(() => '');
    await shot(page, '01-opening-default-period');
    log('期初页首次进入', { picked, openingTotalDebit, stateTag });
    check(
      '期初页首次进入默认期间 = 当前月 2026-11（不是最新的 2026-12）',
      /2026-11/.test(picked.txt),
      picked,
    );
    check(
      '默认期间落在有数据的那一期（合计借方非 0.00，用户不会以为「期初丢了」）',
      openingTotalDebit.trim() !== '' && openingTotalDebit.trim() !== '0.00',
      { openingTotalDebit },
    );

    // 反向对照：2026-12 也是 OPEN，正是老逻辑（取第一个 OPEN）会落到的期间 —— 全 0.00 / 未录
    await pickPeriod(page, '2026-12');
    const decPicked = await selectText(page, 'opening-period-select');
    const decTotalDebit = await page.locator('[data-testid="opening-total-debit"]').innerText().catch(() => '');
    const decState = await page.locator('[data-testid="opening-period-status"]').innerText().catch(() => '');
    await shot(page, '02-opening-period-2026-12-old-default');
    log('手动切到 2026-12（老逻辑的默认落点）', { decPicked, decTotalDebit, decState });
    check(
      '对照：2026-12 是 OPEN 但全空（0.00 / 未录）—— 这就是老默认让人以为数据丢了的原因',
      /2026-12/.test(decPicked.txt) && (decTotalDebit.trim() === '0.00' || /未录/.test(decState)),
      { decTotalDebit, decState },
    );

    // 刷新回默认：重新进页面仍然落当前月（不是记住上一期）
    await page.goto(`${BASE}/business/erp/opening`, { waitUntil: 'domcontentloaded' });
    await hideOnboarding(page);
    await page.waitForTimeout(3000);
    const again = await selectText(page, 'opening-period-select');
    check('重新进入期初页仍默认当前月（不残留 2026-12）', /2026-11/.test(again.txt), again);

    // ================================================== F2 盘点详情凭证号
    await page.goto(`${BASE}/business/erp/stock-take`, { waitUntil: 'domcontentloaded' });
    await page.locator('[data-testid="stocktake-table"]').waitFor({ timeout: 45000 });
    await hideOnboarding(page);
    await page.waitForTimeout(2000);
    await shot(page, '03-stocktake-list');

    const takeNos = await page.locator('[data-testid^="stocktake-no-"]').allInnerTexts();
    const takeNo = (takeNos[0] || '').trim();
    log('盘点列表', { count: takeNos.length, takeNo });
    check('盘点列表有已过账/已作废的单', /^ST-\d{8}-\d{6}$/.test(takeNo), { takeNo });

    await page.locator(`[data-testid="btn-view-${takeNo}"]`).click();
    await page.locator('[data-testid="stocktake-detail-no"]').waitFor({ timeout: 30000 });
    await page.waitForTimeout(1800);
    const detailNo = (await page.locator('[data-testid="stocktake-detail-no"]').innerText()).trim();
    const status = (await page.locator('[data-testid="stocktake-detail-status"]').innerText()).trim();
    const voucherText = (await page.locator('[data-testid="stocktake-detail-voucher"]').innerText()).trim();
    const reverseText = await page
      .locator('[data-testid="stocktake-detail-reverse-voucher"]')
      .innerText()
      .catch(() => '（无红冲凭证字段）');
    await shot(page, '04-stocktake-detail-vouchers');
    log('盘点详情', { detailNo, status, voucherText, reverseText: reverseText.trim() });

    check('差异凭证显示凭证号 PZ-yyyyMMdd-nnnnnn', /^PZ-\d{8}-\d{6}$/.test(voucherText), { voucherText });
    check(
      '红冲凭证显示凭证号 PZR-yyyyMMdd-nnnnnn（不是只修一半）',
      /^PZR-\d{8}-\d{6}$/.test(reverseText.trim()),
      { reverseText: reverseText.trim() },
    );
    check(
      '不再显示 UUID 截断（8 位十六进制 + …）',
      !/^[0-9a-f]{8}…/.test(voucherText) && !/^[0-9a-f]{8}…/.test(reverseText.trim()),
      { voucherText, reverseText: reverseText.trim() },
    );

    // 点凭证号 → 凭证列表按号过滤（财务核账链路）
    // 注意：凭证页的深链设计是「预填筛选项 + 立刻查一次，然后把 voucher_no 从 URL 清掉」
    //（voucher/index.vue:openByQuery 注释，避免刷新又把筛选项改回去），所以判据是
    //「筛选框被预填 + 列表里只有这一张凭证」，不是 URL 上还挂着参数。
    await page.locator(`[data-testid="stocktake-voucher-link-${voucherText}"]`).click();
    await page.waitForTimeout(3500);
    const url1 = page.url();
    const filterNo1 = await page.locator('[data-testid="voucher-filter-no"] input').first().inputValue().catch(() => '');
    const meta1 = await page.locator('[data-testid="voucher-meta"]').innerText().catch(() => '');
    const row1 = await page.locator(`[data-testid="voucher-no-${voucherText}"]`).innerText().catch(() => '');
    await shot(page, '05-voucher-list-from-stocktake');
    log('点差异凭证跳转', { url: url1, filterNo: filterNo1, meta: meta1, row: row1 });
    check(
      '点差异凭证号 → 凭证列表筛选框预填该号、列表只命中这一张（可核账）',
      /\/business\/erp\/voucher$/.test(url1) && filterNo1.trim() === voucherText && row1.includes(voucherText),
      { url: url1, filterNo: filterNo1, meta: meta1, row: row1 },
    );

    // 红冲凭证同样可点（回盘点页重新开详情）
    await page.goto(`${BASE}/business/erp/stock-take`, { waitUntil: 'domcontentloaded' });
    await page.locator('[data-testid="stocktake-table"]').waitFor({ timeout: 45000 });
    await hideOnboarding(page);
    await page.waitForTimeout(1500);
    await page.locator(`[data-testid="btn-view-${takeNo}"]`).click();
    await page.locator('[data-testid="stocktake-detail-no"]').waitFor({ timeout: 30000 });
    await page.waitForTimeout(1200);
    await page.locator(`[data-testid="stocktake-reverse-voucher-link-${reverseText.trim()}"]`).click();
    await page.waitForTimeout(3500);
    const url2 = page.url();
    const filterNo2 = await page.locator('[data-testid="voucher-filter-no"] input').first().inputValue().catch(() => '');
    const row2 = await page
      .locator(`[data-testid="voucher-no-${reverseText.trim()}"]`)
      .innerText()
      .catch(() => '');
    await shot(page, '06-voucher-list-from-stocktake-reverse');
    log('点红冲凭证跳转', { url: url2, filterNo: filterNo2, row: row2 });
    check(
      '点红冲凭证号同样 → 凭证列表预填该号并命中该红字凭证',
      /\/business\/erp\/voucher$/.test(url2) && filterNo2.trim() === reverseText.trim() && row2.includes(reverseText.trim()),
      { url: url2, filterNo: filterNo2, row: row2 },
    );

    // API 侧对照（同实例，页面读的就是这两个字段）
    const api = await page.evaluate(async (takeNo_) => {
      const token = JSON.parse(localStorage.getItem('token') || '{}').access_token
        || JSON.parse(localStorage.getItem('auth') || '{}').access_token
        || null;
      return { tokenPresent: !!token, takeNo_ };
    }, takeNo);
    log('页面 token 探测', api);

    log('===== 本轮结论 =====', {
      round: ROUND,
      total: RESULTS.length,
      pass: RESULTS.filter((r) => r.pass).length,
      fails: RESULTS.filter((r) => !r.pass).map((r) => r.name),
    });
  } catch (err) {
    log('E2E 异常', String((err && err.stack) || err).slice(0, 2000));
    try {
      await shot(page, 'zz-failure');
    } catch (e) {
      /* ignore */
    }
    RESULTS.push({ name: 'e2e-执行未中断', pass: false });
  } finally {
    fs.writeFileSync(
      LOG_PATH,
      LINES.join('\n') + '\n\n## VERDICT\n' + JSON.stringify(RESULTS, null, 2) + '\n',
    );
    await browser.close();
  }
  const fails = RESULTS.filter((r) => !r.pass);
  console.log('FAILS:', JSON.stringify(fails.map((r) => r.name)));
  process.exit(fails.length ? 1 : 0);
})();
