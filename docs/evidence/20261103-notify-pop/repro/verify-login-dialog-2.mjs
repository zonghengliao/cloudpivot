// 登录弹窗：退出登录 → 重新登录又弹 / 全空账号不弹（真机，真后端）
import { chromium } from '@playwright/test';
import fs from 'node:fs';
import path from 'node:path';

const BASE = 'http://127.0.0.1:5290';
const OUT = '/opt/cloudpivot/docs/evidence/20261103-notify-pop';
const SCRATCH = '/root/.hermes/profiles/frontend-developer/cache/scratch/np';
fs.mkdirSync(OUT, { recursive: true });

const results = { stages: {} };
const browser = await chromium.launch();

const newPage = async () => {
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  await ctx.addInitScript(() => localStorage.setItem('cp-onboarding-done', '1'));
  const page = await ctx.newPage();
  page.on('pageerror', (e) => results[`pageerror_${Date.now()}`] = String(e).slice(0, 200));
  return page;
};

const login = async (page, username, password) => {
  await page.goto(`${BASE}/login`);
  await page.getByPlaceholder('请输入用户名').fill(username);
  await page.getByPlaceholder('请输入密码').fill(password);
  await page.getByRole('button', { name: /登\s*录/ }).click();
  await page.waitForURL(/\/dashboard/, { timeout: 30000 });
};

const readCounts = async (page) => {
  const out = {};
  for (const key of ['notice', 'announcement', 'task', 'approval']) {
    out[key] = (await page.locator(`[data-testid="login-unread-count-${key}"]`).innerText()).trim();
  }
  return out;
};

const page = await newPage();

try {
  // ===== D1. np_admin2 登录 → 弹（1 条站内信） =====
  await login(page, 'np_admin2', 'Test@12345');
  await page.waitForSelector('[data-testid="login-unread-row-notice"]', { state: 'visible', timeout: 20000 });
  await page.waitForTimeout(500);
  results.stages.d1 = {
    counts: await readCounts(page),
    title: (await page.locator('.t-dialog__header').first().innerText()).trim(),
  };
  await page.screenshot({ path: path.join(OUT, '04-login-dialog-admin2.png') });

  // ===== D2. 知道了 → 标记 =====
  await page.locator('[data-testid="login-unread-ok"]').click();
  await page.waitForSelector('[data-testid="login-unread-row-notice"]', { state: 'hidden', timeout: 10000 });
  results.stages.d2 = { flagAfterOk: await page.evaluate(() => sessionStorage.getItem('login-unread-shown')) };

  // ===== D3. 退出登录 → 标记被清 =====
  await page.locator('.basic-layout__user').click();
  await page.waitForTimeout(400);
  await page.locator('.t-dropdown__item', { hasText: '退出登录' }).click();
  await page.waitForTimeout(700);
  const confirmTexts = await page.locator('.t-dialog__footer button').allInnerTexts().catch(() => []);
  results.stages.d3_confirmButtons = confirmTexts;
  const confirm = page.locator('.t-dialog__footer button', { hasText: '退出' }).first();
  await confirm.click({ noWaitAfter: true }).catch(async () => {
    await page.getByRole('button', { name: '退出', exact: true }).click({ noWaitAfter: true });
  });
  await page.waitForURL(/\/login/, { timeout: 20000 });
  results.stages.d3 = {
    flagAfterLogout: await page.evaluate(() => sessionStorage.getItem('login-unread-shown')),
    url: new URL(page.url()).pathname,
  };

  // ===== D4. 重新登录 → 又弹一次 =====
  await login(page, 'np_admin2', 'Test@12345');
  await page.waitForSelector('[data-testid="login-unread-row-notice"]', { state: 'visible', timeout: 20000 });
  await page.waitForTimeout(500);
  results.stages.d4 = { counts: await readCounts(page), dialogShownAgain: true };
  await page.screenshot({ path: path.join(OUT, '05-login-dialog-after-relogin.png') });
} catch (error) {
  results.stages.dError = String(error).slice(0, 300);
  await page.screenshot({ path: path.join(OUT, 'diag-d-failure.png') }).catch(() => undefined);
}

// ===== E. 四类全空账号（np_fresh）登录 → 不弹 =====
try {
  const fresh = await newPage();
  await login(fresh, 'np_fresh', 'Test@12345');
  await fresh.waitForTimeout(5000);
  results.stages.e = {
    dialogVisible: await fresh.locator('[data-testid="login-unread-row-notice"]').isVisible(),
    flag: await fresh.evaluate(() => sessionStorage.getItem('login-unread-shown')),
  };
  await fresh.screenshot({ path: path.join(OUT, '06-login-dialog-empty-skip.png') });
} catch (error) {
  results.stages.eError = String(error).slice(0, 300);
}

fs.writeFileSync(path.join(SCRATCH, 'results-login-dialog-2.json'), JSON.stringify(results, null, 2));
console.log(JSON.stringify(results, null, 2));
await browser.close();
