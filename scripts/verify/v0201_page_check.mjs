#!/usr/bin/env node
/**
 * V0201 成本法收敛：主数据弹窗页面验收（Playwright，打隔离实例）。
 *
 * 前置：后端 18097（库 cp_v0201_iso）+ vite 5173（BACKEND_ORIGIN=127.0.0.1:18097）。
 *   cd frontend && node ../scripts/verify/v0201_page_check.mjs
 *
 * 断言只看 DOM（data-testid / class / 文案），截图只做留痕 →
 *   docs/evidence/20261102-v0201/
 */
import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';

const BASE = 'http://127.0.0.1:5173';
const OUT_DIR = '/opt/cloudpivot/docs/evidence/20261102-v0201';

let PASS = 0;
let FAIL = 0;
const ok = (msg) => { PASS += 1; console.log(`[PASS] ${msg}`); };
const bad = (msg) => { FAIL += 1; console.log(`[FAIL] ${msg}`); };
const expect = (name, want, got) => {
  if (String(want) === String(got)) ok(`${name}（${got}）`);
  else bad(`${name}（期望 ${want}，实际 ${got}）`);
};
const expectTrue = (name, cond, got = '') => {
  if (cond) ok(`${name}${got ? `（${got}）` : ''}`);
  else bad(`${name}${got ? `（${got}）` : ''}`);
};

(async () => {
  const loginRes = await fetch(`${BASE}/api/system/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username: 'admin', password: 'Admin@1234567' }),
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
      localStorage.setItem('x_guide_done', '1');
      // OnboardingHint 的「新功能引导」浮层会盖住列表按钮，先标记已看过
      localStorage.setItem('cp-onboarding-done', '1');
    },
    [accessToken, refreshToken],
  );
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', (e) => errors.push(String(e)));

  await page.goto(`${BASE}/business/erp/material`, { waitUntil: 'networkidle' });
  await page.waitForSelector('[data-testid="btn-new-material"]', { timeout: 30000 });
  await page.click('[data-testid="btn-new-material"]');
  await page.waitForSelector('[data-testid="mat-dialog"]', { timeout: 15000 });

  const text = async (sel) => (await page.locator(sel).first().innerText()).trim();

  // ① 弹窗里出现「成本法」表单项，且值 = 移动加权平均（MOVING_AVG）
  const formItems = await page.locator('.t-dialog .t-form__item').allInnerTexts();
  expectTrue('弹窗有「成本法」表单项', formItems.some((t) => t.includes('成本法')), formItems.length + ' 个表单项');
  const costingText = await page.locator('[data-testid="mat-costing"] input').first().inputValue();
  expect('成本法当前值', '移动加权平均（MOVING_AVG）', costingText);

  // ② 下拉是禁用的（本版本只有一项，不给「选了不生效」的机会）
  const disabled = await page.locator('[data-testid="mat-costing"] .t-is-disabled').count();
  expectTrue('成本法下拉置灰（disabled）', disabled >= 1, `t-is-disabled 命中 ${disabled}`);

  // ③ 灰字说明：与后端 400 文案同口径
  const hint = await text('[data-testid="mat-costing-help"]');
  expectTrue('灰字含「本版本仅支持移动加权平均（MOVING_AVG）」', hint.includes('本版本仅支持移动加权平均（MOVING_AVG）'), hint);
  expectTrue('灰字说明 FIFO / STANDARD 在路线图', hint.includes('FIFO') && hint.includes('路线图'), '');
  const hintColor = await page.locator('[data-testid="mat-costing-help"]').evaluate(
    (el) => getComputedStyle(el).color,
  );
  expectTrue('灰字用的是次要色（不是正文黑）', hintColor !== 'rgb(0, 0, 0)', hintColor);

  // ④ 点不开下拉（disabled 元素点击无效果）：证明「只剩一项且不可改」
  await page.click('[data-testid="mat-costing"]', { force: true }).catch(() => {});
  await page.waitForTimeout(400);
  const popupCount = await page.locator('.t-select__dropdown, .t-popup').count();
  expectTrue('禁用下拉点不开选项面板', popupCount === 0, `弹出层命中 ${popupCount}`);

  // ⑤ 新建表单里成本法占位存在时，其它字段仍正常（没被这次改动带坏）
  expectTrue('物料编码输入框在', (await page.locator('[data-testid="mat-code"]').count()) === 1);
  expectTrue('分类下拉在', (await page.locator('[data-testid="mat-form-type"]').count()) === 1);
  expectTrue('一物一码开关在', (await page.locator('[data-testid="mat-serial-switch"]').count()) === 1);

  await page.screenshot({ path: `${OUT_DIR}/material-dialog-costing.png`, fullPage: false });
  ok(`截图已存 ${OUT_DIR}/material-dialog-costing.png`);

  expectTrue('页面无 JS 运行时错误', errors.length === 0, errors.join(' | ').slice(0, 200));

  console.log(`\n结果：${PASS} PASS / ${FAIL} FAIL`);
  await browser.close();
  process.exit(FAIL === 0 ? 0 : 1);
})();
