// 登录未读汇总弹窗真机验收（Playwright + 真后端 8023 + 本地 dist 静态服务 5290）
import { chromium } from '@playwright/test';
import fs from 'node:fs';
import path from 'node:path';

const BASE = 'http://127.0.0.1:5290';
const OUT = '/opt/cloudpivot/docs/evidence/20261103-notify-pop';
const SCRATCH = '/root/.hermes/profiles/frontend-developer/cache/scratch/np';
fs.mkdirSync(OUT, { recursive: true });

const results = { base: BASE, stages: {} };
const consoleErrors = [];

const browser = await chromium.launch();

const newPage = async () => {
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  await ctx.addInitScript(() => localStorage.setItem('cp-onboarding-done', '1'));
  const page = await ctx.newPage();
  page.on('console', (m) => {
    if (m.type() === 'error') consoleErrors.push(m.text().slice(0, 200));
  });
  page.on('pageerror', (e) => consoleErrors.push(`pageerror: ${e.message}`));
  return page;
};

const login = async (page, username, password) => {
  await page.goto(`${BASE}/login`);
  await page.getByPlaceholder('请输入用户名').fill(username);
  await page.getByPlaceholder('请输入密码').fill(password);
  await page.getByRole('button', { name: /登\s*录/ }).click();
  await page.waitForURL(/\/dashboard/, { timeout: 30000 });
};

const dialogReady = async (page) => {
  await page.waitForSelector('[data-testid="login-unread-row-notice"]', { state: 'visible', timeout: 20000 });
};

const readCounts = async (page) => {
  const out = {};
  for (const key of ['notice', 'announcement', 'task', 'approval']) {
    out[key] = (await page.locator(`[data-testid="login-unread-count-${key}"]`).innerText()).trim();
  }
  return out;
};

const stage = async (name, fn) => {
  try {
    results.stages[name] = { ok: true, ...(await fn()) };
  } catch (error) {
    results.stages[name] = { ok: false, error: String(error).slice(0, 400) };
    console.log(`[stage ${name}] FAILED:`, String(error).slice(0, 300));
  }
};

const page = await newPage();

// ===== A. 有未读 → 登录后屏幕中间弹 =====
await stage('A_dialogShown', async () => {
  await login(page, 'admin', 'Admin@123456');
  await dialogReady(page);
  await page.waitForTimeout(800);
  const box = await page.locator('.t-dialog').first().boundingBox();
  const title = (await page.locator('.t-dialog__header').first().innerText()).trim();
  await page.screenshot({ path: path.join(OUT, '01-login-dialog-admin.png') });
  return { counts: await readCounts(page), title, dialogWidth: box?.width, dialogCenterX: box ? box.x + box.width / 2 : null, viewportHalf: 720 };
});

// ===== B. 四类「查看」各跳对应页面 =====
for (const [key, expected] of Object.entries({
  notice: '/business/notice/message',
  announcement: '/announcement',
  task: '/task/list',
  approval: '/flow/task',
})) {
  await stage(`B_nav_${key}`, async () => {
    await page.evaluate(() => sessionStorage.removeItem('login-unread-shown'));
    await page.goto(`${BASE}/dashboard`);
    await dialogReady(page);
    await page.locator(`[data-testid="login-unread-view-${key}"]`).click();
    await page.waitForURL((url) => url.pathname === expected, { timeout: 20000 });
    if (key === 'notice') await page.screenshot({ path: path.join(OUT, '02-login-dialog-view-notice.png') });
    return { expected, actual: new URL(page.url()).pathname };
  });
}

// ===== C. 「知道了」→ 刷新不再弹 =====
await stage('C_closeAndReload', async () => {
  await page.evaluate(() => sessionStorage.removeItem('login-unread-shown'));
  await page.goto(`${BASE}/dashboard`);
  await dialogReady(page);
  await page.locator('[data-testid="login-unread-ok"]').click();
  await page.waitForSelector('[data-testid="login-unread-row-notice"]', { state: 'hidden', timeout: 10000 });
  const flag = await page.evaluate(() => sessionStorage.getItem('login-unread-shown'));
  await page.reload();
  await page.waitForTimeout(3000);
  const visible = await page.locator('[data-testid="login-unread-row-notice"]').isVisible();
  await page.screenshot({ path: path.join(OUT, '03-login-dialog-after-reload.png') });
  return { flagAfterOk: flag, visibleAfterReload: visible };
});

// ===== D. 退出登录 → 重新登录 → 又弹（标记被清） =====
await stage('D_logoutThenRelogin', async () => {
  await page.goto(`${BASE}/dashboard`);
  await page.waitForTimeout(500);
  await page.locator('.basic-layout__user').click({ timeout: 8000 });
  await page.locator('.t-dropdown__item', { hasText: '退出登录' }).click();
  await page.getByRole('button', { name: '退出', exact: true }).click();
  await page.waitForURL(/\/login/, { timeout: 20000 });
  const flag = await page.evaluate(() => sessionStorage.getItem('login-unread-shown'));
  await login(page, 'admin', 'Admin@123456');
  await dialogReady(page);
  await page.screenshot({ path: path.join(OUT, '04-login-dialog-after-relogin.png') });
  return { flagAfterLogout: flag, counts: await readCounts(page) };
});

// ===== E. 四类全空 → 不弹：把夹具公告下线，用新账号 np_fresh 真机登录 =====
await stage('E_emptyAccountNoDialog', async () => {
  const token = await page.evaluate(() => localStorage.getItem('x_access_token'));
  const annId = fs.readFileSync(path.join(SCRATCH, 'ann_id.txt'), 'utf8').trim();
  const offline = await page.evaluate(
    async ({ token, annId }) => {
      const r = await fetch(`/api/business/announcement/${annId}/offline`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${token}` },
        body: JSON.stringify({ reason: '验收夹具下线（NP 收尾）' }),
      });
      return { status: r.status, body: await r.json() };
    },
    { token, annId },
  );
  const fresh = await newPage();
  await login(fresh, 'np_fresh', 'Test@12345');
  await fresh.waitForTimeout(4500);
  const visible = await fresh.locator('[data-testid="login-unread-row-notice"]').isVisible();
  await fresh.screenshot({ path: path.join(OUT, '05-login-dialog-empty-skip.png') });
  return { offlineStatus: offline.status, dialogVisible: visible, flag: await fresh.evaluate(() => sessionStorage.getItem('login-unread-shown')) };
});

results.consoleErrors = consoleErrors;
fs.writeFileSync(path.join(SCRATCH, 'results-login-dialog.json'), JSON.stringify(results, null, 2));
console.log(JSON.stringify(results, null, 2));
await browser.close();
