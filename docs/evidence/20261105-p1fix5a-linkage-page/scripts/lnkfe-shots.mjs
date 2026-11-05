/**
 * 联动规则配置页真机验收截图（卡 t_d0daa4f0 / P1-FIX-5A）。
 *
 * 前置：
 *   - 隔离后端 127.0.0.1:18121（库 cloudpivot_lnkfe，`/root/lnkfe-instance.sh start`）
 *   - vite dev 5197：`cd frontend && BACKEND_ORIGIN=http://127.0.0.1:18121 pnpm dev --port 5197 --strictPort`
 *
 * 产出（docs/evidence/20261105-p1fix5a-linkage-page/shots/）：
 *   01-list.png        列表（真实规则 + 白名单动作 / 事务归属 / 启停）
 *   02-create-dialog.png  新增弹窗（动作白名单下拉 + 动态参数 + 二次确认）
 *   03-preview.png     保存前的影响预览（草稿校验通过 + 回滚风险说明）
 *   04-edit-dialog.png 编辑弹窗（三项唯一键只读）
 *   05-preview-dialog.png 既有触发点的执行链预览
 */
import { chromium } from '@playwright/test';
import { mkdirSync } from 'node:fs';
import { resolve } from 'node:path';

const BASE = process.env.BASE_URL || 'http://127.0.0.1:5197';
const SHOTS = resolve('/opt/cloudpivot/docs/evidence/20261105-p1fix5a-linkage-page/shots');
const USER = process.env.ADMIN_USER || 'admin';
const PASS = process.env.ADMIN_PASS || 'LinkFe@654321';

mkdirSync(SHOTS, { recursive: true });

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1500, height: 950 }, deviceScaleFactor: 2 });
const errors = [];
page.on('console', (msg) => {
  if (msg.type() === 'error') {
    errors.push(`console: ${msg.text()}`);
  }
});
page.on('pageerror', (error) => errors.push(`pageerror: ${error.message}`));

async function shot(name) {
  await page.waitForTimeout(400);
  await page.screenshot({ path: resolve(SHOTS, name), fullPage: false });
  console.log('shot', name);
}

// ---------- 登录 ----------
await page.goto(`${BASE}/login`, { waitUntil: 'domcontentloaded' });
await page.getByPlaceholder('请输入用户名').fill(USER);
await page.getByPlaceholder('请输入密码').fill(PASS);
await page.getByRole('button', { name: /登\s*录/ }).click();
await page.waitForURL(/dashboard/, { timeout: 20000 });
console.log('logged in');

// ---------- 列表 ----------
await page.goto(`${BASE}/flow/linkage`, { waitUntil: 'domcontentloaded' });
await page.waitForSelector('[data-testid="linkage-table"]', { timeout: 20000 });
// 新功能引导会盖住页面（只提示一次，localStorage 记忆）：先关掉再操作
const hint = page.locator('.onboarding__close');
if ((await hint.count()) > 0) {
  await hint.first().click({ timeout: 5000 }).catch(() => {});
}
await page.waitForTimeout(800);
const rows = await page.locator('[data-testid="linkage-table"] tbody tr').count();
console.log('rows', rows);
await shot('01-list.png');

// ---------- 新增弹窗 ----------
await page.locator('[data-testid="btn-linkage-create"]').click();
await page.waitForSelector('[data-testid="linkage-form-name"]');
await page.locator('[data-testid="linkage-form-name"] input, input[data-testid="linkage-form-name"]').first().fill('收货过账后通知仓管（真机演示）');

// 源单据：收货入库单
await page.locator('[data-testid="linkage-form-entity"]').click();
await page.locator('.t-select-option', { hasText: '收货入库单' }).first().click();
// 触发状态：POSTED
await page.locator('[data-testid="linkage-form-status"]').click();
await page.locator('.t-select-option', { hasText: 'POSTED' }).first().click();
// 动作：发通知
await page.locator('[data-testid="linkage-form-action"]').click();
await page.locator('.t-select-option', { hasText: 'SEND_NOTICE' }).first().click();
await page.waitForTimeout(600);

// 参数：mode=TAKEOVER（枚举）+ title
await page.locator('.params .t-select').first().click();
await page.locator('.t-select-option', { hasText: 'TAKEOVER' }).first().click();
await page.locator('.params input.t-input__inner').nth(1).fill('收货入库单已过账');
await page.waitForTimeout(300);
await shot('02-create-dialog.png');

// 影响预览
await page.locator('[data-testid="linkage-form-preview"]').click();
await page.waitForSelector('text=这条规则可以通过校验', { timeout: 20000 });
// 预览结果在内容区底部：滚到底再截，保证校验结论 + 提醒 + 何时生效都在画面里
await page.locator('.editor').evaluate((el) => {
  el.scrollTop = el.scrollHeight;
});
await page.waitForTimeout(500);
await shot('03-preview.png');

// 勾选二次确认并创建
await page.locator('[data-testid="linkage-form-confirm"]').click();
const confirmBtn = page.locator('.t-dialog__footer .t-button--theme-primary').last();
await confirmBtn.click();
await page.waitForSelector('text=规则已创建', { timeout: 20000 });
await page.waitForTimeout(1200);
await shot('01b-list-after-create.png');

// ---------- 编辑弹窗 ----------
await page.waitForTimeout(3200); // 等「规则已创建」toast 消失，别糊在截图里
await page.locator('[data-testid="linkage-edit"]').first().click();
await page.waitForSelector('[data-testid="linkage-form-name"]');
await page.locator('.editor').evaluate((el) => {
  el.scrollTop = 0;
});
await page.waitForTimeout(600);
await shot('04-edit-dialog.png');
await page.locator('.t-dialog__footer .t-button').first().click(); // 取消
await page.waitForTimeout(400);

// ---------- 既有触发点的影响预览 ----------
await page.locator('[data-testid="linkage-preview"]').first().click();
await page.waitForSelector('text=执行影响预览', { timeout: 20000 });
await page.waitForTimeout(1200);
await shot('05-preview-dialog.png');

console.log('errors', JSON.stringify(errors.filter((item) => !item.includes('favicon')), null, 2));
await browser.close();
