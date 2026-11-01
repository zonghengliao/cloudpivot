#!/usr/bin/env node
/**
 * ERP-R2 利润表页面验收（Playwright，打**隔离实例** http://127.0.0.1:5189）。
 *
 * 前置：先在仓根起隔离实例（prod 库副本 + 后端 18089 + vite 5189）
 *
 *     E2E_ISO_SRC=local bash scripts/verify/e2e_isolated_instance.sh up
 *
 * 用法（从 frontend/ 目录跑，方便解析 node_modules）：
 *
 *     cd frontend && node ../scripts/verify/erpr2_page_check.mjs ok
 *     cd frontend && node ../scripts/verify/erpr2_page_check.mjs broken
 *
 *   ok     —— 库里有一笔 借 1002 / 贷 5001 1,000 的收入：
 *             勾稽条必须是绿框、diff 0.00、净利润 1,000.00、13 行、合计行加粗；
 *   broken —— 库里再有一笔 借 1002 / 贷 5111 250 的投资收益（利润表没有这一行）：
 *             勾稽条必须是红框、diff −250.00、提示点名 5111、UNMAPPED_PROFIT_LOSS 行 250.00。
 *
 * 断言只看 DOM（类名 / data-testid / 背景色 / 文案）；截图只做留痕
 * （落 `docs/evidence/20261101-erpr2/`）。
 */
import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';

const MODE = process.argv[2] || 'ok';
const BASE = 'http://127.0.0.1:5189';
const OUT_DIR = '/opt/cloudpivot/docs/evidence/20261101-erpr2';

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
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 } });
  await context.addInitScript(
    ([a, r]) => {
      localStorage.setItem('x_access_token', a);
      localStorage.setItem('x_refresh_token', r);
      // 关掉首屏引导卡（与 e2e/global-setup.ts 同一个键）
      localStorage.setItem('x_guide_done', '1');
    },
    [accessToken, refreshToken],
  );
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));

  await page.goto(`${BASE}/business/erp/report/income-statement`, { waitUntil: 'networkidle' });
  await page.waitForSelector('[data-testid="is-check"]', { timeout: 30000 });

  const text = async (sel) => (await page.locator(sel).first().innerText()).trim();
  expect('页面标题', '利润表', await text('.is-head__title'));
  expect('期间', '期间：2026年10月（未结账）', await text('[data-testid="is-period-name"]'));
  expect('本年累计起点展示', '本年累计：2026-01-01 起', await text('[data-testid="is-ytd-start"]'));
  expect('报表行数（12 + 未归类行）', '13', await page.locator('[data-testid^="is-row-"]').count());
  expect('表头第一列', '项目', await text('.is-table thead th:nth-child(1)'));
  expect('表头第二列', '本期金额', await text('.is-table thead th:nth-child(2)'));
  expect('表头第三列', '本年累计', await text('.is-table thead th:nth-child(3)'));
  expect(
    '营业收入带「一、」前缀',
    '一、营业收入',
    await text('[data-testid="is-row-0"] td:nth-child(1)'),
  );
  expect(
    '营业成本带「减：」前缀',
    '减：营业成本',
    await text('[data-testid="is-row-1"] td:nth-child(1)'),
  );

  const checkClass = await page.locator('[data-testid="is-check"]').getAttribute('class');
  const bg = await page.locator('[data-testid="is-check"]').evaluate((el) => getComputedStyle(el).backgroundColor);
  const diff = await text('[data-testid="is-check-diff"]');
  const netYtd = await text('[data-testid="is-net-profit-ytd"]');
  const equity = await text('[data-testid="is-equity-change"]');

  if (MODE === 'ok') {
    expect('勾稽条类名含 is-check--ok', true, checkClass.includes('is-check--ok'));
    expect('勾稽条底色是绿系', 'rgb(227, 249, 233)', bg);
    expect('净利润（累计）', '1,000.00', netYtd);
    expect('未分配利润本年变动', '1,000.00', equity);
    expect('差额', '0.00', diff);
    expect('本期营业收入', '1,000.00', await text('[data-testid="is-current-REVENUE"]'));
    expect('累计营业收入', '1,000.00', await text('[data-testid="is-ytd-REVENUE"]'));
    expect('累计净利润', '1,000.00', await text('[data-testid="is-ytd-NET_PROFIT"]'));
    expect('合计行加了 is-row--total（净利润行 = 第 13 行）', true,
      (await page.locator('[data-testid="is-row-12"]').getAttribute('class')).includes('is-row--total'));
    expect('未归类损益行单独一档样式（第 12 行）', true,
      (await page.locator('[data-testid="is-row-11"]').getAttribute('class')).includes('is-row--unmapped'));
    await page.screenshot({ path: `${OUT_DIR}/20-页面-勾稽一致.png`, fullPage: true });
    console.log(`  截图：${OUT_DIR}/20-页面-勾稽一致.png`);
  } else {
    // 红框场景：一笔「投资收益 5111 250」进了未分配利润、却没进任何利润表行
    expect('勾稽条类名含 is-check--bad', true, checkClass.includes('is-check--bad'));
    expect('勾稽条底色是红系', 'rgb(255, 240, 237)', bg);
    expect('差额', '-250.00', diff);
    expect('净利润（累计）', '1,000.00', netYtd);
    expect('未分配利润本年变动', '1,250.00', equity);
    const hints = await page.locator('.is-check__hints li').allInnerTexts();
    expect('提示里点名 5111 投资收益', true, hints.join(' ').includes('5111'));
    const unmappedList = await page.locator('[data-testid="is-unmapped-list"]').innerText();
    expect('未归类科目清单里显示 5111', true, unmappedList.includes('5111'));
    expect('报表出现 UNMAPPED_PROFIT_LOSS 行金额 250.00', '250.00',
      await text('[data-testid="is-ytd-UNMAPPED_PROFIT_LOSS"]'));
    const message = await text('[data-testid="is-check-line"]');
    expect('结论是「不平 + 差额」', true, message.includes('不平') && message.includes('-250.00'));
    await page.screenshot({ path: `${OUT_DIR}/21-页面-勾稽不平.png`, fullPage: true });
    console.log(`  截图：${OUT_DIR}/21-页面-勾稽不平.png`);
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
