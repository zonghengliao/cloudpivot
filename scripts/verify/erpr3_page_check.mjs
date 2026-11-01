#!/usr/bin/env node
/**
 * ERP-R3 现金流量表页面验收（Playwright，打**隔离实例** http://127.0.0.1:15173）。
 *
 * 前置（先起实例，两种模式各起一次）：
 *
 *     bash scripts/verify/erpr3_page_up.sh ok
 *     cd frontend && node ../scripts/verify/erpr3_page_check.mjs ok
 *     bash scripts/verify/erpr3_page_up.sh broken
 *     cd frontend && node ../scripts/verify/erpr3_page_check.mjs broken
 *
 *   ok     —— 7 张正常凭证：勾稽条绿框、差额 0.00、期末 14,400.00 = 货币资金 14,400.00、
 *             经营 1,900.00 / 投资 −1,500.00 / 筹资 4,000.00、无黄条；
 *   broken —— 再补一张「借 1012 / 贷 1002 200」（1012 不在现金类口径里）：
 *             顶部黄条 ≥1 条且点名 1012、勾稽条红框、差额 −200.00、期末 14,200.00、
 *             「其他」分区出现 200.00 的流出。
 *
 * 断言只看 DOM（类名 / data-testid / 背景色 / 文案）；截图只做留痕
 * （落 `docs/evidence/20261101-erpr3/`）。
 */
import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';

const MODE = process.argv[2] || 'ok';
const BASE = process.env.E2E_BASE_URL || 'http://127.0.0.1:15173';
const OUT_DIR = '/opt/cloudpivot/docs/evidence/20261101-erpr3';

let PASS = 0;
let FAIL = 0;
const ok = (msg) => { PASS += 1; console.log(`[PASS] ${msg}`); };
const bad = (msg) => { FAIL += 1; console.log(`[FAIL] ${msg}`); };
const expect = (name, want, got) => {
  if (String(want) === String(got)) ok(`${name}（${got}）`);
  else bad(`${name}（期望 ${want}，实际 ${got}）`);
};

(async () => {
  const loginRes = await fetch(`${BASE}/api/system/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username: 'laoliu_test', password: 'Test@12345' }),
  });
  const loginBody = await loginRes.json();
  if (loginBody.code !== 0) {
    console.error('登录失败：', JSON.stringify(loginBody).slice(0, 200));
    process.exit(1);
  }
  const { access_token: accessToken, refresh_token: refreshToken } = loginBody.data;

  const browser = await chromium.launch();
  const context = await browser.newContext({ viewport: { width: 1440, height: 1200 } });
  await context.addInitScript(
    ([a, r]) => {
      localStorage.setItem('x_access_token', a);
      localStorage.setItem('x_refresh_token', r);
      localStorage.setItem('x_guide_done', '1');
    },
    [accessToken, refreshToken],
  );
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));

  await page.goto(`${BASE}/business/erp/report/cash-flow`, { waitUntil: 'networkidle' });
  await page.waitForSelector('[data-testid="cf-check"]', { timeout: 30000 });

  const text = async (sel) => (await page.locator(sel).first().innerText()).trim();
  const attr = async (sel, name) => await page.locator(sel).first().getAttribute(name);
  const bg = async (sel) =>
    await page.locator(sel).first().evaluate((el) => getComputedStyle(el).backgroundColor);

  // ---------- 公共断言 ----------
  expect('页面标题', '现金流量表', await text('.cf-head__title'));
  expect('期间展示', '期间：2026年10月（未结账）', await text('[data-testid="cf-period-name"]'));
  expect('本年累计起点', '本年累计：2026-01-01 起', await text('[data-testid="cf-ytd-start"]'));
  expect('表头第一列', '项目', await text('.cf-table thead th:nth-child(1)'));
  expect('表头第二列', '本期金额', await text('.cf-table thead th:nth-child(2)'));
  expect('表头第三列', '本年累计', await text('.cf-table thead th:nth-child(3)'));
  expect(
    '简化口径写在页面上（口径跟着报表走）',
    true,
    (await text('[data-testid="cf-simplified"]')).includes('简化版'),
  );

  // 三大活动分区标题
  expect(
    '经营分区标题',
    '一、经营活动产生的现金流量',
    await text('[data-testid="cf-section-OPERATING"] td:nth-child(1)'),
  );
  expect(
    '投资分区标题',
    '二、投资活动产生的现金流量',
    await text('[data-testid="cf-section-INVESTING"] td:nth-child(1)'),
  );
  expect(
    '筹资分区标题',
    '三、筹资活动产生的现金流量',
    await text('[data-testid="cf-section-FINANCING"] td:nth-child(1)'),
  );

  const checkClass = await attr('[data-testid="cf-check"]', 'class');
  const checkBg = await bg('[data-testid="cf-check"]');
  const diff = await text('[data-testid="cf-check-diff"]');
  const closing = await text('[data-testid="cf-closing-current"]');
  const bsCash = await text('[data-testid="cf-check-bs-cash"]');
  const netIncrease = await text('[data-testid="cf-net-increase-current"]');
  const opening = await text('[data-testid="cf-opening-current"]');
  const operatingNet = await text('[data-testid="cf-net-current-OPERATING"]');
  const investingNet = await text('[data-testid="cf-net-current-INVESTING"]');
  const financingNet = await text('[data-testid="cf-net-current-FINANCING"]');

  if (MODE === 'ok') {
    expect('勾稽条类名含 is-check--ok', true, checkClass.includes('is-check--ok'));
    expect('勾稽条底色是绿系', 'rgb(227, 249, 233)', checkBg);
    expect('差额', '0.00', diff);
    expect('期末现金余额', '14,400.00', closing);
    expect('资产负债表货币资金', '14,400.00', bsCash);
    expect('净增加额', '4,400.00', netIncrease);
    expect('期初余额', '10,000.00', opening);
    expect('经营净额', '1,900.00', operatingNet);
    expect('投资净额', '-1,500.00', investingNet);
    expect('筹资净额', '4,000.00', financingNet);
    expect('本期销售商品收到的现金', '5,000.00', await text('[data-testid="cf-current-OPER_SALES_RECEIPTS"]'));
    expect('本期购买商品支付的现金', '2,000.00', await text('[data-testid="cf-current-OPER_PURCHASE_PAYMENTS"]'));
    expect('本期支付职工薪酬', '800.00', await text('[data-testid="cf-current-OPER_STAFF_PAYMENTS"]'));
    expect('本期支付各项税费', '300.00', await text('[data-testid="cf-current-OPER_TAX_PAYMENTS"]'));
    expect('本期购建长期资产', '1,500.00', await text('[data-testid="cf-current-INVEST_PURCHASES"]'));
    expect('本期取得借款', '4,000.00', await text('[data-testid="cf-current-FIN_LOANS_IN"]'));
    expect('累计吸收投资（9 月那笔）', '10,000.00', await text('[data-testid="cf-ytd-FIN_CAPITAL_IN"]'));
    expect('累计经营净额', '1,900.00', await text('[data-testid="cf-net-ytd-OPERATING"]'));
    expect('一致时没有黄条', 0, await page.locator('[data-testid="cf-warn"]').count());
    expect('一致时未分类 0 笔', true, (await text('[data-testid="cf-check-unclassified"]')).includes('0'));
    expect('「其他」分区不展示（没有未分类收支时）', 0, await page.locator('[data-testid="cf-section-OTHER"]').count());
    expect('勾稽结论是「勾稽一致」', true, (await text('[data-testid="cf-check-line"]')).includes('勾稽一致'));
    await page.screenshot({ path: `${OUT_DIR}/30-页面-现金流量表-勾稽一致.png`, fullPage: true });
    console.log(`  截图：${OUT_DIR}/30-页面-现金流量表-勾稽一致.png`);
  } else {
    // 破：借 1012 / 贷 1002 200 进了「其他」，期末现金 14,200 而货币资金 14,400
    expect('勾稽条类名含 is-check--bad', true, checkClass.includes('is-check--bad'));
    expect('勾稽条底色是红系', 'rgb(255, 240, 237)', checkBg);
    expect('差额', '-200.00', diff);
    expect('期末现金余额', '14,200.00', closing);
    expect('资产负债表货币资金', '14,400.00', bsCash);
    expect('净增加额', '4,200.00', netIncrease);
    expect('结论是「不平 + 差额」', true,
      (await text('[data-testid="cf-check-line"]')).includes('不平'));
    const warnCount = await page.locator('[data-testid="cf-warn"]').count();
    expect('顶部黄条出现（warnings 非空）', 1, warnCount);
    const warnText = warnCount ? await text('[data-testid="cf-warn"]') : '';
    expect('黄条点名「未能分类」', true, warnText.includes('未能分类'));
    expect('黄条点名 1012', true, warnText.includes('1012'));
    const hintText = (await page.locator('.cf-check__hints li').allInnerTexts()).join(' ');
    expect('勾稽提示点名 1012 口径差异', true, hintText.includes('1012'));
    expect('「其他」分区出现（未分类收支必须看得见）', 1,
      await page.locator('[data-testid="cf-section-OTHER"]').count());
    expect('「其他」分区净额 = −200.00', '-200.00', await text('[data-testid="cf-net-current-OTHER"]'));
    expect('未分类 1 笔', true, (await text('[data-testid="cf-check-unclassified"]')).includes('1'));
    await page.screenshot({ path: `${OUT_DIR}/31-页面-现金流量表-未分类与口径不平.png`, fullPage: true });
    console.log(`  截图：${OUT_DIR}/31-页面-现金流量表-未分类与口径不平.png`);
  }

  if (errors.length) bad(`页面 JS 报错：${errors.join(' | ')}`);
  else ok('页面无 JS 报错');

  await browser.close();
  console.log(`\n  模式=${MODE}  PASS=${PASS}  FAIL=${FAIL}`);
  process.exit(FAIL === 0 ? 0 : 1);
})().catch((e) => {
  console.error('页面验收异常：', e);
  process.exit(1);
});
