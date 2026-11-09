#!/usr/bin/env node
/**
 * ERP-P1 会计期间页验收（Playwright，打**生产/隔离实例**的 nginx 入口）。
 *
 * ```bash
 * cd /opt/cloudpivot/frontend && node ../scripts/verify/erp1_period_page_check.mjs
 * # 换地址：E2E_BASE_URL=http://127.0.0.1:15173 node ../scripts/verify/erp1_period_page_check.mjs
 * ```
 *
 * **只读**：全程不点「确认结账 / 确认反结账」，只打开结账弹窗看预检结果（那是 GET close-check）。
 *
 * 断言只看 DOM（data-testid / 类名 / 文案 / disabled 属性）：
 * 1. 期间列表出现 2026-10 / 2026-11；
 * 2. 顶部「开账」「批量开账」按钮存在（admin 有三码）；
 * 3. `OPEN` 行有「结账」动作，且**没有**「反结账」（当前没有 CLOSED 期间）；
 * 4. 点「结账」→ 弹窗出现 + 预检返回阻塞项（2026-10 有草稿收货单 / 草稿采购单），
 *    文案点名张数，确认按钮 disabled（结不掉）；
 * 5. 全程没有 console error / 未捕获异常。
 *
 * 环境变量：`E2E_ADMIN_PASSWORD`（默认从 `/etc/cloudpivot/cloudpivot.env` 导出的 NIK_ADMIN_PASSWORD）。
 */
import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';

const BASE = process.env.E2E_BASE_URL || 'http://127.0.0.1';
// 登录口径（卡 t_d4e92b99）：运维账号 CLOUDPIVOT_OPS_*，单次尝试
const USER = process.env.E2E_ADMIN_USER || process.env.CLOUDPIVOT_OPS_USER || 'admin';
const PASSWORD =
  process.env.CLOUDPIVOT_OPS_PASSWORD || process.env.E2E_ADMIN_PASSWORD || process.env.NIK_ADMIN_PASSWORD || '';

let PASS = 0;
let FAIL = 0;
const ok = (msg) => {
  PASS += 1;
  console.log(`[PASS] ${msg}`);
};
const bad = (msg) => {
  FAIL += 1;
  console.log(`[FAIL] ${msg}`);
};

(async () => {
  if (!PASSWORD) {
    console.error('缺口令：CLOUDPIVOT_OPS_PASSWORD / E2E_ADMIN_PASSWORD');
    process.exit(2);
  }

  const loginRes = await fetch(`${BASE}/api/system/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username: USER, password: PASSWORD }),
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
      // 新功能引导（`components/OnboardingHint.vue` 的 STORAGE_KEY）：存 1 就不再弹，
      // 否则它的遮罩会拦住「结账」按钮的点击（首次点击超时）
      localStorage.setItem('cp-onboarding-done', '1');
    },
    [accessToken, refreshToken],
  );
  const page = await context.newPage();
  const errors = [];
  page.on('console', (msg) => {
    if (msg.type() === 'error') errors.push(msg.text());
  });
  page.on('pageerror', (error) => errors.push(String(error)));

  await page.goto(`${BASE}/business/erp/period`, { waitUntil: 'networkidle' });
  await page.waitForSelector('[data-testid="period-table"]', { timeout: 20000 });

  // ① 列表里有本期两个期间
  const codes = await page.$$eval('.period-code', (nodes) => nodes.map((n) => n.textContent.trim()));
  if (codes.includes('2026-10') && codes.includes('2026-11')) ok(`期间列表出现 2026-10 / 2026-11（${codes.join(' / ')}）`);
  else bad(`期间列表缺 2026-10 或 2026-11（实际 ${codes.join(' / ')}）`);

  // ② 顶部开账 / 批量开账（admin 有 period:open）
  const openBtn = await page.$('[data-testid="btn-period-open"]');
  const batchBtn = await page.$('[data-testid="btn-period-batch-open"]');
  if (openBtn && batchBtn) ok('顶部「开账」「批量开账」按钮存在');
  else bad(`顶部按钮缺失（开账=${!!openBtn} 批量=${!!batchBtn}）`);

  // ③ OPEN 行有「结账」，没有「反结账」
  const closeLink = await page.$('[data-testid="btn-period-close-2026-10"]');
  const reopenLink = await page.$('[data-testid="btn-period-reopen-2026-10"]');
  if (closeLink && !reopenLink) ok('2026-10（OPEN）有「结账」且无「反结账」');
  else bad(`动作按状态显示不对（结账=${!!closeLink} 反结账=${!!reopenLink}）`);

  // ④ 点结账 → 预检弹窗（只读，不确认）
  await closeLink.click();
  await page.waitForSelector('[data-testid="period-close-dialog"]', { timeout: 20000 });
  await page.waitForFunction(
    () =>
      document.querySelector('[data-testid="period-close-check-blocked"]') ||
      document.querySelector('[data-testid="period-close-check-ok"]'),
    { timeout: 20000 },
  );
  const blocked = await page.$('[data-testid="period-close-check-blocked"]');
  // 预检文案在 t-alert 的 description 插槽里（`<ul data-testid="period-close-blockers">`），
  // 但 TDesign 的插槽渲染层级不稳定，直接读**整个弹窗**的文本更可靠。
  const dialogText = (await page.$eval('[data-testid="period-close-dialog"]', (n) => n.textContent || ''))
    .replace(/\s+/g, ' ')
    .trim();
  if (blocked && /还有 \d+ 张单据未完成/.test(dialogText)) {
    const matched = dialogText.match(/还有 \d+ 张单据未完成[^。]*/);
    ok(`结账预检点名阻塞项：${(matched ? matched[0] : dialogText).slice(0, 80)}`);
  } else if (blocked) {
    bad(`预检弹窗出现了阻塞条，但文案没点名张数：「${dialogText.slice(0, 140)}」`);
  } else {
    bad('预检没有出现阻塞条（2026-10 生产上确实有草稿单据，应拦住）');
  }

  // 确认按钮必须 disabled（结不掉）
  const confirmDisabled = await page.evaluate(() => {
    const dialog = document.querySelector('[data-testid="period-close-dialog"]');
    if (!dialog) return null;
    const buttons = [...dialog.querySelectorAll('button')];
    const target = buttons.find((b) => (b.textContent || '').includes('确认结账'));
    return target ? target.disabled : null;
  });
  if (confirmDisabled === true) ok('有阻塞项时「确认结账」是 disabled（结不掉）');
  else bad(`「确认结账」按钮状态不对（disabled=${confirmDisabled}）`);

  // 关掉弹窗（取消），不执行任何写操作
  await page.keyboard.press('Escape');

  // ⑤ 无 console error
  const realErrors = errors.filter((text) => !/favicon|Download the Vue Devtools/i.test(text));
  if (realErrors.length === 0) ok('无 console error / 未捕获异常');
  else bad(`有 ${realErrors.length} 条 console error：${realErrors.slice(0, 2).join(' | ')}`);

  await browser.close();

  console.log(`\n合计 ${PASS + FAIL} 项：PASS ${PASS} / FAIL ${FAIL}`);
  process.exit(FAIL === 0 ? 0 : 1);
})().catch((error) => {
  console.error('验收脚本异常：', error);
  process.exit(1);
});
