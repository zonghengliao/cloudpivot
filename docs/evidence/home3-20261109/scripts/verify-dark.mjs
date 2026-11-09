/**
 * 深色模式 + 抽屉截图（补充证据）：
 *   · 切到深色后页面是否仍然可读（环、状态灯、折线、卡片表面）；
 *   · 「新功能引导」已按真实行为标记为看过，不遮挡。
 */
import { chromium } from '@playwright/test';
import { mkdirSync } from 'node:fs';

const BASE = process.env.BASE || 'http://127.0.0.1:5399';
const OUT = '/var/tmp/home3-evidence/ui';
mkdirSync(OUT, { recursive: true });

const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1680, height: 2300 } });
await ctx.addInitScript(() => {
  try {
    localStorage.setItem('cp-onboarding-done', '1');
  } catch {}
});
const page = await ctx.newPage();
await page.goto(`${BASE}/login`, { waitUntil: 'domcontentloaded' });
await page.getByPlaceholder('请输入用户名').fill('admin');
await page.getByPlaceholder('请输入密码').fill(process.env.ADMIN_PASSWORD || 'Home3@2026Pass');
await page.locator('button[type="submit"]').first().click();
await page.waitForURL(/\/dashboard/, { timeout: 20000 });
await page.waitForTimeout(1000);
const skip = page.getByTestId('setup-wizard-skip');
if (await skip.count()) {
  await skip.first().click({ timeout: 5000 }).catch(() => {});
  await page.waitForTimeout(600);
}
await page.goto(`${BASE}/system/server-status`, { waitUntil: 'domcontentloaded' });
await page.waitForSelector('.ss-metric', { timeout: 20000 });
await page.waitForTimeout(1200);

// 顶栏的月亮按钮 = 切深色（选择器用组件类名，避免按图标猜）
const themeBtn = page.locator('.basic-layout__theme-toggle');
await themeBtn.click({ timeout: 8000 });
await page.waitForTimeout(900);
const mode = await page.evaluate(() => document.documentElement.getAttribute('theme-mode'));
console.log('theme-mode after click:', mode);
await page.waitForTimeout(800);
await page.screenshot({ path: `${OUT}/08-dark-mode.png`, fullPage: true });
console.log('dark screenshot saved, theme-mode =', mode);
// 切回浅色，避免影响后续人工验收
await themeBtn.click().catch(() => {});
await page.waitForTimeout(500);
await browser.close();
