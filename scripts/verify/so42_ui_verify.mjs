/**
 * SO-42 真机验收（Chromium + vite dev + 真后端）
 *
 * 验三件事（对应卡面验收「手工验证维护币种/汇率 + 单据能选币种」）：
 *  1. 币种管理页：种子 4 币种在列 → 弹窗新建 THB → 行出现在表里
 *  2. 汇率管理页：列表显示 SPOT/MONTH_END 行 → 「按月批量录入」弹窗按当前月回填已录值 →
 *     改值提交 → 列表出现新值
 *  3. 销售订单页：新建弹窗有「币种」选择器；选 USD 后「汇率」只读显示按单据日取到的 9.99
 *     （= 后端建单锁定的同一个值），且汇率框是文本不是输入框
 *
 * 跑法（仓库外）：
 *   ln -sfn /opt/cloudpivot/frontend/node_modules node_modules
 *   node verify.mjs
 */
import { chromium } from '@playwright/test';
import fs from 'node:fs';
import path from 'node:path';

const BASE = 'http://127.0.0.1:5188';
const OUT = '/opt/cloudpivot/docs/evidence/20261107-so42-currency';
fs.mkdirSync(OUT, { recursive: true });
// 每次跑都重开证据文件（追加模式会把上一轮的红字一起留在里面）
fs.writeFileSync(path.join(OUT, 'ui_evidence.txt'), '');

const log = (...args) => {
  const line = args.map((a) => (typeof a === 'string' ? a : JSON.stringify(a))).join(' ');
  console.log(line);
  fs.appendFileSync(path.join(OUT, 'ui_evidence.txt'), line + '\n');
};

const login = async () => {
  const res = await fetch(`${BASE}/api/system/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username: 'admin', password: 'Admin@123456' }),
  });
  const body = await res.json();
  if (body.code !== 0) throw new Error(`登录失败: ${JSON.stringify(body)}`);
  return body.data;
};

/** 在组件里找 input（t-input / t-input-number 的 testid 落在外层，值在内部 input 上） */
const fillByTestId = async (page, testid, value) => {
  const host = page.locator(`[data-testid="${testid}"]`).first();
  const input = host.locator('input').first();
  const target = (await input.count()) > 0 ? input : host;
  await target.click();
  await target.press('Control+a');
  await target.press('Backspace');
  await target.type(String(value), { delay: 20 });
  await target.press('Tab');
  return target.inputValue().catch(() => '');
};

const main = async () => {
  const tokens = await login();
  log(`== 登录成功，token 长度 ${tokens.access_token.length}`);

  const browser = await chromium.launch();
  const context = await browser.newContext({ viewport: { width: 1680, height: 1000 } });
  await context.addInitScript(
    ([access, refresh]) => {
      localStorage.setItem('x_access_token', access);
      localStorage.setItem('x_refresh_token', refresh);
      localStorage.setItem('cp-onboarding-done', '1');
      sessionStorage.setItem('login-unread-shown', '1');
    },
    [tokens.access_token, tokens.refresh_token],
  );
  const page = await context.newPage();
  const consoleErrors = [];
  page.on('console', (msg) => {
    if (msg.type() === 'error') consoleErrors.push(msg.text());
  });

  // ==================== ① 币种管理 ====================
  await page.goto(`${BASE}/business/erp/currency`);
  await page.waitForSelector('[data-testid="currency-table"] tbody tr', { timeout: 30000 });
  await page.waitForFunction(
    () => (document.querySelector('[data-testid="currency-table"]')?.textContent ?? '').includes('CNY'),
    { timeout: 20000 },
  );
  const currencyText = await page.locator('[data-testid="currency-table"]').innerText();
  log('== ① 币种列表（段落）:');
  log(currencyText.split('\n').slice(0, 14).join(' | '));
  for (const code of ['CNY', 'USD', 'EUR', 'HKD', 'JPY']) {
    if (!currencyText.includes(code)) throw new Error(`币种列表缺 ${code}`);
  }
  if (!currencyText.includes('本位币')) throw new Error('列表没有「本位币」标记');
  const baseTagCount = await page.locator('[data-testid="currency-base-tag"]').count();
  log(`== 本位币标记数量 = ${baseTagCount}（期望 1）`);
  if (baseTagCount !== 1) throw new Error('本位币标记必须唯一');
  await page.screenshot({ path: path.join(OUT, 'ui-01-currency-list.png'), fullPage: true });

  // 新建 THB
  await page.locator('[data-testid="btn-currency-new"]').click();
  await page.waitForSelector('[data-testid="currency-form-dialog"]', { timeout: 10000 });
  await fillByTestId(page, 'currency-form-code', 'THB');
  await fillByTestId(page, 'currency-form-name', '泰铢');
  await fillByTestId(page, 'currency-form-symbol', '฿');
  await page.screenshot({ path: path.join(OUT, 'ui-02-currency-create-dialog.png') });
  const dialog = page.locator('.t-dialog').filter({ hasText: '新建币种' }).last();
  await dialog.getByRole('button', { name: '创建', exact: true }).click();
  await page.waitForFunction(
    () => (document.querySelector('[data-testid="currency-table"]')?.textContent ?? '').includes('THB'),
    { timeout: 20000 },
  );
  log('== ① 新建 THB 成功（表里已出现）');
  // 等弹窗收起（TDesign 关闭有动画，抢拍会拍到半透明残留）
  await page.waitForTimeout(1600);
  await page.screenshot({ path: path.join(OUT, 'ui-03-currency-created.png'), fullPage: true });

  // ==================== ② 汇率管理 ====================
  await page.goto(`${BASE}/business/erp/exchange-rate`);
  await page.waitForSelector('[data-testid="rate-table"] tbody tr', { timeout: 30000 });
  await page.waitForFunction(
    () => (document.querySelector('[data-testid="rate-table"]')?.textContent ?? '').includes('USD'),
    { timeout: 20000 },
  );
  const rateText = await page.locator('[data-testid="rate-table"]').innerText();
  log('== ② 汇率列表（段落）:');
  log(rateText.split('\n').slice(0, 16).join(' | '));
  if (!rateText.includes('USD') || !rateText.includes('2026-11-30')) {
    throw new Error('汇率列表缺 USD 的 2026-11-30 月末行');
  }
  await page.screenshot({ path: path.join(OUT, 'ui-04-rate-list.png'), fullPage: true });

  // 按月批量录入
  await page.locator('[data-testid="btn-rate-batch"]').click();
  await page.waitForSelector('[data-testid="rate-batch-table"]', { timeout: 15000 });
  await page.waitForTimeout(1200);
  const batchEffective = await page.locator('[data-testid="rate-batch-effective"]').innerText();
  const batchText = await page.locator('[data-testid="rate-batch-table"]').innerText();
  log(`== ② 批量录入弹窗：生效日=${batchEffective}`);
  log(`== 批量录入行（前 10 段）: ${batchText.split('\n').slice(0, 10).join(' | ')}`);
  if (!batchEffective.includes('2026-11-30')) throw new Error('批量弹窗的月末生效日不对');
  await page.screenshot({ path: path.join(OUT, 'ui-05-rate-batch-dialog.png') });

  // 把 USD 改成 7.25 再提交（upsert 覆盖）
  await fillByTestId(page, 'rate-batch-input-USD', '7.25000000');
  const batchDialog = page.locator('.t-dialog').filter({ hasText: '按月批量录入汇率' }).last();
  await batchDialog.getByRole('button', { name: '批量写入', exact: true }).click();
  await page.waitForFunction(
    () => (document.querySelector('[data-testid="rate-table"]')?.textContent ?? '').includes('7.25000000'),
    { timeout: 20000 },
  );
  log('== ② 批量写入 7.25 成功（列表已刷新）');
  await page.waitForTimeout(1600);
  await page.screenshot({ path: path.join(OUT, 'ui-06-rate-after-batch.png'), fullPage: true });

  // ==================== ③ 销售订单：币种选择器 + 只读汇率 ====================
  await page.goto(`${BASE}/business/erp/sales-order`);
  await page.waitForSelector('[data-testid="btn-so-new"]', { timeout: 30000 });
  await page.locator('[data-testid="btn-so-new"]').click();
  await page.waitForSelector('[data-testid="so-form-currency"]', { timeout: 15000 });
  // 汇率只读：容器里没有 input
  const rateInputs = await page.locator('[data-testid="so-form-rate"] input').count();
  log(`== ③ 汇率区 input 数量 = ${rateInputs}（期望 0：只读）`);
  if (rateInputs !== 0) throw new Error('汇率必须是只读展示，不应有输入框');
  const beforeSelect = await page.locator('[data-testid="so-form-rate"]').innerText();
  log(`== ③ 默认（本位币）汇率区: ${beforeSelect.replace(/\n/g, ' / ')}`);

  // 选 USD
  await page.locator('[data-testid="so-form-currency"]').click();
  await page.waitForTimeout(600);
  const option = page.locator('.t-select-option:visible', { hasText: '美元' }).first();
  await option.click();
  await page.waitForFunction(
    () => (document.querySelector('[data-testid="so-form-rate"]')?.textContent ?? '').includes('9.99'),
    { timeout: 20000 },
  );
  const afterSelect = await page.locator('[data-testid="so-form-rate"]').innerText();
  log(`== ③ 选 USD 后汇率区: ${afterSelect.replace(/\n/g, ' / ')}`);
  const currencyValue = await page.locator('[data-testid="so-form-currency"] input').first().inputValue();
  log(`== ③ 币种选择器当前值 = ${currencyValue}`);
  await page.screenshot({ path: path.join(OUT, 'ui-07-sales-order-currency.png') });

  log(`== 控制台 error 条数 = ${consoleErrors.length}`);
  if (consoleErrors.length > 0) log(`（前 5 条）${consoleErrors.slice(0, 5).join(' ｜ ')}`);

  await browser.close();
  log('== UI 验收全部通过');
};

main().catch((error) => {
  log(`!! 失败: ${error.message}`);
  process.exit(1);
});
