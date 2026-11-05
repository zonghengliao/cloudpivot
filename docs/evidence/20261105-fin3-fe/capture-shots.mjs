/**
 * FIN-3-FE 真机截图脚本（一次性，验收产物，不入库）。
 *
 * 用 Playwright 独立起一个 chromium：browser_use 的 CDP 通道在长会话后
 * 截图会间歇超时，截图这件事不必依赖它。
 *
 * 登录走接口拿 token，再写进 localStorage（key 与 `src/api/request.ts` 的
 * `TOKEN_KEY` 一致）——**不碰密码输入框**，也不在脚本里存凭据。
 */
import { chromium } from '@playwright/test';
import { mkdirSync } from 'node:fs';

const BASE = 'http://127.0.0.1:5190';
const OUT = '/opt/cloudpivot/docs/evidence/20261105-fin3-fe';
mkdirSync(OUT, { recursive: true });

const login = await fetch('http://127.0.0.1:18027/api/system/auth/login', {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ username: 'admin', password: 'Admin@123456' }),
}).then((r) => r.json());

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
page.on('pageerror', (err) => console.log('PAGEERROR', err.message));

await page.goto(`${BASE}/login`);
await page.evaluate((token) => localStorage.setItem('x_access_token', token), login.data.access_token);

async function shot(path, file, prepare) {
  await page.goto(`${BASE}${path}`);
  await page.waitForTimeout(2000);
  // 一次性 onboarding 提示会盖住表格右侧并拦截点击 —— 直接摘掉节点
  await page.evaluate(() => document.querySelectorAll('[data-onboarding-hint]').forEach((n) => n.remove()));
  await page.waitForTimeout(500);
  if (prepare) {
    await prepare();
    await page.waitForTimeout(1200);
  }
  await page.screenshot({ path: `${OUT}/${file}`, fullPage: false });
  console.log('shot', file);
}

await shot('/business/erp/expense', '01-expense-list.png', async () => {
  // 关掉一次性引导浮层，别挡表格
  const know = page.locator('button:has-text("知道了")').first();
  if (await know.isVisible().catch(() => false)) await know.click();
});

await shot('/business/erp/expense', '02-expense-form.png', async () => {
  await page.locator('[data-testid="expense-btn-create"]').click();
  await page.locator('[data-testid="expense-form-title"] input').fill('FE 验收：差旅 + 业务招待');
  // 第一行：差旅费 3180 / 6%
  await page.locator('[data-testid="expense-item-type-0"]').click();
  await page.locator('.t-select-option[title="差旅费（TRAVEL）"]:visible').click();
  await page.locator('[data-testid="expense-item-amount-0"] input').fill('3180');
  await page.locator('[data-testid="expense-item-rate-0"]').click();
  await page.locator('.t-select-option[title="6%（服务 / 差旅）"]:visible').click();
  await page.locator('[data-testid="expense-item-0"] .items__cell:nth-child(6) input').fill('高铁票');
  await page.locator('[data-testid="expense-item-0"] .items__cell:nth-child(7) input').fill('INV-FE-0001');
  // 第二行：业务招待费 1060（税额置灰 0 + 提示）
  await page.locator('button:has-text("添加明细")').click();
  await page.waitForTimeout(400);
  await page.locator('[data-testid="expense-item-type-1"]').click();
  await page.locator('.t-select-option[title="业务招待费（MEAL · 不可抵扣）"]:visible').click();
  await page.locator('[data-testid="expense-item-amount-1"] input').fill('1060');
  await page.locator('[data-testid="expense-item-1"] .items__cell:nth-child(6) input').fill('客户餐费');
  await page.locator('[data-testid="expense-item-1"] .items__cell:nth-child(7) input').fill('INV-FE-0002');
});

await shot('/business/erp/expense', '03-expense-detail.png', async () => {
  const rows = page.locator('tr', { hasText: 'EXP-20261105-000004' });
  await rows.locator('button:has-text("详情")').click();
  await page.waitForSelector('[data-testid="expense-detail-body"]');
  await page.waitForTimeout(800);
  // 滚到「预算占用 / 付款记录 / 审批轨迹 / 凭证号」那一段
  await page.evaluate(() => {
    const body = document.querySelector('[data-testid="expense-detail-body"]');
    const scroller = body.closest('.t-drawer__body') || body.parentElement;
    scroller.scrollTop = scroller.scrollHeight;
  });
});

await shot('/business/erp/expense/pay', '04-pay-list.png', async () => {
  const know = page.locator('button:has-text("知道了")').first();
  if (await know.isVisible().catch(() => false)) await know.click();
});

await shot('/business/erp/expense/pay', '05-pay-dialog.png', async () => {
  await page.locator('button:has-text("付款登记")').first().click();
  await page.waitForSelector('[data-testid="expense-pay-dialog"]');
});

await shot('/business/erp/expense-type', '06-type-list.png', async () => {
  const know = page.locator('button:has-text("知道了")').first();
  if (await know.isVisible().catch(() => false)) await know.click();
});

await shot('/business/erp/expense-type', '07-type-dialog.png', async () => {
  await page.locator('[data-testid="expense-type-btn-create"]').click();
  await page.waitForSelector('[data-testid="expense-type-dialog"]');
});

await browser.close();
console.log('DONE');
