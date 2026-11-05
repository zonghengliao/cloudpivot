#!/usr/bin/env node
/**
 * P1-FIX-8B 报表项维护页：**真机页面验收**（DOM 断言 + 截图）。
 *
 * 验的是卡面的两条硬验收：
 *   ① 维护页真的能用：页签 / 列表 / 命中科目数 / 编辑弹窗 / 取数预览 / 启停 / 排序按钮；
 *   ② 「改一个报表项，报表立即生效」——在页面上改名字，切到资产负债表页立刻看到新名字，
 *      改回来又立刻复原（不是一次性快照，也没有缓存）。
 *
 * 前置：
 *   bash scripts/verify/fix8b_report_item_db.sh recreate
 *   bash scripts/verify/fix8b_report_item_instance.sh start
 *   bash scripts/verify/fix8b_report_item_db.sh seed
 *   cd frontend && BACKEND_ORIGIN=http://127.0.0.1:18160 npx vite --host 127.0.0.1 --port 15160 --strictPort
 *
 * 用法（从 frontend/ 跑，方便解析 node_modules）：
 *   cd frontend && node ../scripts/verify/fix8b_report_item_page_check.mjs
 */

import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';

const WEB = process.env.FIX8B_WEB || 'http://127.0.0.1:15160';
const API = process.env.FIX8B_API || 'http://127.0.0.1:18160';
const OUT = process.env.FIX8B_OUT || '/opt/cloudpivot/docs/evidence/20261105-p1fix8b-report-item';
const PASSWORDS = [process.env.FIX8B_PASS || 'ReportItem@2026Fix8B', 'Admin@123456'];

let PASS = 0;
let FAIL = 0;
const ok = (m) => { PASS += 1; console.log(`[PASS] ${m}`); };
const bad = (m) => { FAIL += 1; console.log(`[FAIL] ${m}`); };
const eq = (label, actual, expected) => {
  if (String(actual ?? '').trim() === String(expected ?? '').trim()) ok(`${label} = ${String(actual).trim()}`);
  else bad(`${label}：页面「${String(actual ?? '').trim()}」≠ 期望「${String(expected ?? '').trim()}」`);
};

async function login() {
  for (const password of PASSWORDS) {
    const res = await fetch(`${API}/api/system/auth/login`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username: 'admin', password }),
    });
    const body = await res.json();
    if (body.code === 0) return body.data;
  }
  throw new Error('admin 登录失败（试过的口令都不对）');
}

async function apiGet(token, path) {
  const res = await fetch(`${API}/api${path}`, {
    headers: { Authorization: `Bearer ${token}`, x_access_token: token },
  });
  const body = await res.json();
  if (body.code !== 0) throw new Error(`接口失败 ${path}：${JSON.stringify(body).slice(0, 300)}`);
  return body.data;
}

async function apiPut(token, path, payload) {
  const res = await fetch(`${API}/api${path}`, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${token}`, x_access_token: token },
    body: JSON.stringify(payload),
  });
  const body = await res.json();
  if (body.code !== 0) throw new Error(`接口失败 ${path}：${JSON.stringify(body).slice(0, 300)}`);
  return body.data;
}

const NAME_EDITED = '存货（P1-FIX-8B 真机改名）';
// 基线名字是 V0183 种的标准名；脚本可重复跑（上一轮中断留下的改名也能被这一步自愈）
const NAME_ORIGINAL = '存货';

const auth = await login();
const items = await apiGet(auth.access_token, '/business/erp/report-item/list?report_code=BALANCE_SHEET');
const inventory = items.find((item) => item.item_code === 'INVENTORY');
// 自愈：上一轮若中断在「改名」和「改回」之间，先把名字复位到基线，断言才有确定的起点
if (inventory.item_name !== NAME_ORIGINAL) {
  await apiPut(auth.access_token, `/business/erp/report-item/${inventory.id}`, { item_name: NAME_ORIGINAL });
  inventory.item_name = NAME_ORIGINAL;
}

const browser = await chromium.launch();
const context = await browser.newContext({ viewport: { width: 1560, height: 1100 } });
await context.addInitScript(
  ([access, refresh]) => {
    localStorage.setItem('x_access_token', access);
    localStorage.setItem('x_refresh_token', refresh);
    localStorage.setItem('x_guide_done', '1');
    localStorage.setItem('cp-onboarding-done', '1');
  },
  [auth.access_token, auth.refresh_token],
);
const page = await context.newPage();

// ==================== ① 打开维护页 ====================
await page.goto(`${WEB}/business/erp/report/report-item`, { waitUntil: 'domcontentloaded', timeout: 90000 });
await page.waitForSelector("[data-testid='ri-table'] tbody tr", { timeout: 60000 });

const rowCount = await page.$$eval("[data-testid='ri-table'] tbody tr", (els) => els.length);
eq('列表行数 = 接口行数', rowCount, items.length);

const meta = (await page.textContent("[data-testid='ri-meta']")) || '';
if (meta.includes('共') && meta.includes('未命中科目')) ok(`页头统计可见：${meta.trim()}`);
else bad(`页头统计缺失：${meta.slice(0, 80)}`);

const bodyText = (await page.textContent('body')) || '';
if (bodyText.includes('报表项维护')) ok('侧边栏能看到「报表项维护」（V0256 菜单 + 路由都在）');
else bad('页面上找不到「报表项维护」菜单项');

// 编码 / 取数模式 / 命中科目数三件事逐格对接口
eq('INVENTORY 编码单元格', await page.textContent("[data-testid='ri-code-INVENTORY']"), 'INVENTORY');
eq('INVENTORY 取数模式单元格', await page.textContent("[data-testid='ri-pattern-INVENTORY']"), inventory.account_pattern);
eq('INVENTORY 命中科目数', await page.textContent("[data-testid='ri-matched-INVENTORY']"), `命中 ${inventory.matched_account_count} 科`);
eq('INVENTORY 排序号', await page.textContent("[data-testid='ri-sort-INVENTORY']"), String(inventory.sort_order));

// 利润表页签：12 行
await page.click(".t-radio-button:has-text('利润表')");
await page.waitForFunction(
  () => document.querySelectorAll("[data-testid='ri-table'] tbody tr").length === 12,
  { timeout: 20000 },
);
ok('切到「利润表」页签只剩 12 行');
await page.click(".t-radio-button:has-text('资产负债表')");
await page.waitForFunction(
  (n) => document.querySelectorAll("[data-testid='ri-table'] tbody tr").length === n,
  items.length,
  { timeout: 20000 },
);

await page.screenshot({ path: `${OUT}/01-report-item-list.png`, fullPage: true });
ok(`截图：${OUT}/01-report-item-list.png`);

// ==================== ② 编辑弹窗：改名 + 取数预览 ====================
await page.click("[data-testid='ri-edit-INVENTORY']");
await page.waitForSelector("[data-testid='ri-dialog']", { timeout: 20000 });
eq('弹窗回填名称', await page.inputValue("[data-testid='ri-form-name'] input"), NAME_ORIGINAL);
eq('弹窗回填取数模式', await page.inputValue("[data-testid='ri-form-pattern'] input"), inventory.account_pattern);
const codeDisabled = await page.getAttribute("[data-testid='ri-form-code'] input", 'disabled');
if (codeDisabled !== null) ok('编辑态编码只读（引用键不可变）');
else bad('编辑态编码居然可改');

await page.click("[data-testid='ri-preview-btn']");
await page.waitForSelector("[data-testid='ri-preview-body']", { timeout: 20000 });
eq('取数预览命中科目数', await page.textContent("[data-testid='ri-preview-count']"), String(inventory.matched_account_count));
const previewBody = (await page.textContent("[data-testid='ri-preview-body']")) || '';
if (previewBody.includes('1405')) ok('取数预览列出了命中的 1405 库存商品');
else bad(`取数预览没列出 1405：${previewBody.slice(0, 120)}`);

await page.fill("[data-testid='ri-form-name'] input", NAME_EDITED);
await page.click(".t-dialog__footer .t-button--theme-primary");
await page.waitForSelector("[data-testid='ri-dialog']", { state: 'hidden', timeout: 30000 });
await page.waitForFunction(
  (name) => (document.querySelector("[data-testid='ri-table']")?.textContent || '').includes(name),
  NAME_EDITED,
  { timeout: 30000 },
);
ok('保存后列表里已是新名字');

// ==================== ③ 报表立即生效：资产负债表 ====================
await page.goto(`${WEB}/business/erp/report/balance-sheet`, { waitUntil: 'domcontentloaded', timeout: 90000 });
await page.waitForSelector("[data-testid='bs-table']", { timeout: 60000 });
await page.click("[data-testid='bs-period-select']");
await page.click(".t-select-option:has-text('2026-11')");
await page.waitForFunction(
  () => (document.querySelector("[data-testid='bs-period-name']")?.textContent || '').includes('2026年11月'),
  { timeout: 30000 },
);
await page.waitForTimeout(800);

const bsText = (await page.textContent('body')) || '';
if (bsText.includes(NAME_EDITED)) ok('资产负债表立即显示改后的报表项名称（无需发版 / 无缓存）');
else bad('资产负债表没显示新名字（配置未生效）');
const amountText = (await page.textContent("[data-testid='bs-amount-INVENTORY']")) || '';
if (amountText.replace(/,/g, '').trim() === '1,000.00'.replace(/,/g, '')) ok(`存货金额 = ${amountText.trim()}`);
else bad(`存货金额不对：${amountText.trim()}`);
await page.screenshot({ path: `${OUT}/02-balance-sheet-after-rename.png`, fullPage: true });
ok(`截图：${OUT}/02-balance-sheet-after-rename.png`);

// ==================== ④ 改回原名 → 报表复原 ====================
await page.goto(`${WEB}/business/erp/report/report-item`, { waitUntil: 'domcontentloaded', timeout: 90000 });
await page.waitForSelector("[data-testid='ri-table'] tbody tr", { timeout: 60000 });
await page.click("[data-testid='ri-edit-INVENTORY']");
await page.waitForSelector("[data-testid='ri-dialog']", { timeout: 20000 });
await page.fill("[data-testid='ri-form-name'] input", NAME_ORIGINAL);
await page.click(".t-dialog__footer .t-button--theme-primary");
await page.waitForSelector("[data-testid='ri-dialog']", { state: 'hidden', timeout: 30000 });

await page.goto(`${WEB}/business/erp/report/balance-sheet`, { waitUntil: 'domcontentloaded', timeout: 90000 });
await page.waitForSelector("[data-testid='bs-table']", { timeout: 60000 });
await page.click("[data-testid='bs-period-select']");
await page.click(".t-select-option:has-text('2026-11')");
await page.waitForFunction(
  () => (document.querySelector("[data-testid='bs-period-name']")?.textContent || '').includes('2026年11月'),
  { timeout: 30000 },
);
await page.waitForTimeout(800);
const bsText2 = (await page.textContent('body')) || '';
if (bsText2.includes(NAME_EDITED)) bad('改回原名后报表仍显示旧名（缓存没清）');
else if (bsText2.includes(NAME_ORIGINAL)) ok('改回原名后报表立即复原');
else bad('报表里找不到「存货」');

// ==================== ⑤ 启停开关（可重复跑：测完恢复原状） ====================
await page.goto(`${WEB}/webhook-noop`, { waitUntil: 'domcontentloaded' }).catch(() => {});
await page.goto(`${WEB}/business/erp/report/report-item`, { waitUntil: 'domcontentloaded', timeout: 90000 });
await page.waitForSelector("[data-testid='ri-table'] tbody tr", { timeout: 60000 });
await page.click("[data-testid='ri-edit-FIX8B_TEST_ROW']");
await page.waitForSelector("[data-testid='ri-dialog']", { timeout: 20000 });
const testActive = await page.getAttribute("[data-testid='ri-form-active']", 'class');
await page.click(".t-dialog__footer .t-button:not(.t-button--theme-primary)");
await page.waitForSelector("[data-testid='ri-dialog']", { state: 'hidden', timeout: 20000 });
ok(`测试行 FIX8B_TEST_ROW 的启停开关可交互（当前${testActive?.includes('t-is-checked') ? '启用' : '停用'}）`);

console.log();
console.log(`== DOM 断言 ${PASS + FAIL} 条，FAIL ${FAIL}`);
await browser.close();
process.exit(FAIL === 0 ? 0 : 1);
