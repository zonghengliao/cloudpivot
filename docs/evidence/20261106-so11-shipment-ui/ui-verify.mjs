/**
 * SO-11 发货单页真机验收（Playwright + vite dev 5191 + 隔离后端 18099）
 *
 * 验的是**页面**，不是接口（接口已在 curl-e2e.sh 里两轮跑过）：
 *   ① 列表页打开、新建按钮可用
 *   ② 新建弹窗：选订单 → **自动带出未发完明细**（数量默认未发量）→ 选仓库 → 创建
 *   ③ 详情抽屉：草稿 → 提交待过账 → 待过账
 *   ④ 过账：确认弹窗正文 → 确认 → 已过账 + **关联出库单号**可见
 *   ⑤ 库内核对：三本账扣减、订单 delivered_qty / 状态（页面之外的真结果）
 *   ⑥ 列表回读：状态=已过账
 */
import { chromium } from '@playwright/test';
import { execFileSync } from 'node:child_process';
import { readFileSync, mkdirSync } from 'node:fs';

const BASE = process.env.APP_BASE || 'http://127.0.0.1:5191';
const API = process.env.API_BASE || 'http://127.0.0.1:18099';
const DB = process.env.DB || 'cloudpivot_so11';
const OUT = process.env.OUT_DIR || '/root/.hermes/profiles/frontend-developer/cache/scratch/so11-verify/shots';
mkdirSync(OUT, { recursive: true });

const env = JSON.parse(readFileSync(`${OUT}/../fixture.json`, 'utf8'));

function psql(sql) {
  return execFileSync('psql', ['-h', '127.0.0.1', '-U', 'alun', '-d', DB, '-tAq', '-c', sql], {
    env: { ...process.env, PGPASSWORD: 'alun' },
    encoding: 'utf8',
  }).trim();
}

async function login() {
  for (const password of ['Admin@123456', 'Admin@20261106']) {
    const res = await fetch(`${API}/api/system/auth/login`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ username: 'admin', password }),
    });
    const body = await res.json();
    if (body.code === 0) {
      return body.data;
    }
  }
  throw new Error('登录失败（两个口令都试过）');
}

const results = [];
function check(label, actual, expected) {
  const ok = String(actual) === String(expected);
  results.push({ label, actual: String(actual), expected: String(expected), ok });
  console.log(`${ok ? '  ✓' : '  ✗'} ${label} = ${actual}${ok ? '' : `（期望 ${expected}）`}`);
}
function note(label, value) {
  results.push({ label, actual: String(value), expected: '(记录)', ok: true });
  console.log(`  · ${label} = ${value}`);
}

const tokens = process.env.ACCESS_TOKEN
  ? { access_token: process.env.ACCESS_TOKEN, refresh_token: '' }
  : await login();
const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1600, height: 1000 } });

await page.addInitScript(
  ([access, refresh]) => {
    localStorage.setItem('x_access_token', access);
    localStorage.setItem('x_refresh_token', refresh ?? '');
    localStorage.setItem('cp-onboarding-done', '1');
    sessionStorage.setItem('login-unread-shown', '1');
  },
  [tokens.access_token, tokens.refresh_token],
);

/** 选中 t-select 里的某个选项（下拉 teleport 到 body，取可见面板） */
async function pickOption(selectTestId, text) {
  await page.getByTestId(selectTestId).click();
  const panel = page.locator('.t-select__dropdown').filter({ has: page.locator('.t-select-option') });
  const option = page.locator('.t-select-option:visible').filter({ hasText: text }).first();
  await option.waitFor({ state: 'visible', timeout: 15000 });
  await option.click();
  await page.waitForTimeout(150);
}

/** 等表格里出现某段文本（t-body 空态也有一行，所以按内容等） */
async function waitTableText(testid, text) {
  await page.waitForFunction(
    ([id, needle]) =>
      (document.querySelector(`[data-testid="${id}"]`)?.textContent ?? '').includes(needle),
    [testid, text],
    { timeout: 20000 },
  );
}

try {
  // ==================== ① 列表页 ====================
  await page.goto(`${BASE}/business/erp/shipment`, { waitUntil: 'domcontentloaded' });
  await page.waitForSelector('[data-testid="shipment-table"]', { timeout: 30000 });
  check('页面标题卡出现', await page.locator('.panel-head__title').innerText(), '发货单');
  const newBtn = page.getByTestId('btn-shipment-new');
  check('新建按钮可点（超管有 create 权限）', await newBtn.isEnabled(), 'true');
  await page.screenshot({ path: `${OUT}/01-list.png`, fullPage: true });

  // ==================== ② 新建：选订单 → 带出明细 ====================
  await page.getByTestId('btn-shipment-new').click();
  await page.waitForSelector('[data-testid="shipment-form-dialog"]', { timeout: 15000 });

  await pickOption('shipment-form-so', env.so_no);
  await waitTableText('shipment-form-items', env.material_label);
  const lineCount = await page.evaluate(() =>
    [...document.querySelectorAll('[data-testid="shipment-form-items"] tbody tr')].filter((tr) =>
      tr.textContent.includes('M-W1A-002'),
    ).length,
  );
  check('自动带出未发完的行数', lineCount, 1);
  const qtyInput = page.locator('[data-testid="shipment-form-qty"] input').first();
  check('本次发货数量默认 = 未发量', await qtyInput.inputValue(), '10');
  check('合计数量', (await page.getByTestId('shipment-form-total').innerText()).replace(/\s/g, ''), '本次合计10');

  await pickOption('shipment-form-warehouse', 'WH01');
  await page.screenshot({ path: `${OUT}/02-create-dialog.png`, fullPage: true });

  await page.getByRole('button', { name: '创建发货单', exact: true }).click();
  await page.waitForSelector('[data-testid="shipment-detail-no"]', { timeout: 25000 });
  const shipNo = (await page.getByTestId('shipment-detail-no').innerText()).trim();
  note('建单成功单号', shipNo);
  check('新建后状态（详情徽标）', (await page.getByTestId('shipment-detail-status').innerText()).trim(), '草稿');
  check('详情里来源订单号', (await page.getByTestId('shipment-detail-desc').innerText()).includes(env.so_no), 'true');
  check('详情里关联出库单号（未过账）', (await page.getByTestId('shipment-detail-stock-out-no').innerText()).trim(), '未过账');

  // ==================== ③ 提交待过账 ====================
  await page.getByTestId('btn-shipment-detail-submit').click();
  await page.waitForFunction(
    () => document.querySelector('[data-testid="shipment-detail-status"]')?.textContent?.includes('待过账'),
    { timeout: 20000 },
  );
  check('提交后状态', (await page.getByTestId('shipment-detail-status').innerText()).trim(), '待过账');
  check('提交不动库存（成本账仍 10）', psql(`SELECT qty::numeric(18,4) FROM biz_inventory WHERE material_id='${env.material_id}'`), '10.0000');

  // ==================== ④ 过账（确认弹窗） ====================
  await page.getByTestId('btn-shipment-detail-post').click();
  await page.waitForSelector('[data-testid="shipment-post-dialog"]', { timeout: 15000 });
  const postText = await page.getByTestId('shipment-post-text').innerText();
  note('过账确认弹窗正文', postText.slice(0, 80) + '…');
  check('弹窗点名单号', postText.includes(shipNo), 'true');
  await page.screenshot({ path: `${OUT}/03-post-dialog.png`, fullPage: true });

  await page.getByTestId('btn-shipment-post-confirm').click();
  await page.waitForFunction(
    () => document.querySelector('[data-testid="shipment-detail-status"]')?.textContent?.includes('已过账'),
    { timeout: 30000 },
  );
  check('过账后状态', (await page.getByTestId('shipment-detail-status').innerText()).trim(), '已过账');
  const stockOutNo = (await page.getByTestId('shipment-detail-stock-out-no').innerText()).trim();
  note('详情显示的关联出库单号', stockOutNo);
  check('出库单号格式 OUT-yyyyMMdd-nnnnnn', /^OUT-\d{8}-\d{6}$/.test(stockOutNo), 'true');
  check(
    '已过账提示可见',
    (await page.getByTestId('shipment-posted-hint').innerText()).includes(stockOutNo),
    'true',
  );
  await page.screenshot({ path: `${OUT}/04-detail-posted.png`, fullPage: true });

  // ==================== ⑤ 库内核对（页面之外的真结果） ====================
  const shipId = psql(`SELECT id FROM biz_shipment WHERE ship_no='${shipNo}'`);
  check('成本账（10−10）', psql(`SELECT COALESCE(SUM(qty),0)::numeric(18,4) FROM biz_inventory WHERE material_id='${env.material_id}'`), '0.0000');
  check('库位账（10−10）', psql(`SELECT COALESCE(SUM(qty),0)::numeric(18,4) FROM biz_inventory_location WHERE material_id='${env.material_id}'`), '0.0000');
  check('出库流水（−10 的绝对值合计）', psql(`SELECT COALESCE(SUM(qty),0)::numeric(18,4) FROM biz_inventory_txn WHERE material_id='${env.material_id}' AND doc_type='STOCK_OUT'`), '10.0000');
  check('销售出库单来源 = SALES', psql(`SELECT source_type FROM biz_stock_out WHERE source_id='${shipId}'`), 'SALES');
  check('销售出库单号与页面一致', psql(`SELECT out_no FROM biz_stock_out WHERE source_id='${shipId}'`), stockOutNo);
  check('成本结转凭证已生成', psql(`SELECT (voucher_id IS NOT NULL)::text FROM biz_stock_out WHERE source_id='${shipId}'`), 'true');
  check('订单行 delivered_qty', psql(`SELECT delivered_qty::numeric(18,4) FROM biz_sales_order_item WHERE so_id='${env.so_id}'`), '10.0000');
  check('订单状态', psql(`SELECT status FROM biz_sales_order WHERE id='${env.so_id}'`), 'OUTBOUND');

  // ==================== ⑥ 列表回读 ====================
  await page.getByTestId('btn-shipment-drawer-close').click();
  await page.getByTestId('btn-shipment-refresh').click();
  await waitTableText('shipment-table', shipNo);
  const rowText = await page.evaluate((no) => {
    const tr = [...document.querySelectorAll('[data-testid="shipment-table"] tbody tr')].find((el) =>
      el.textContent.includes(no),
    );
    return tr?.textContent ?? '';
  }, shipNo);
  check('列表行状态=已过账', rowText.includes('已过账'), 'true');
  check('列表行关联出库单列（后端未下发 → 见详情）', rowText.includes('见详情'), 'true');
  await page.screenshot({ path: `${OUT}/05-list-after.png`, fullPage: true });

  const failed = results.filter((item) => !item.ok);
  console.log(`\n===== UI 断言：${results.length - failed.length}/${results.length} 通过，失败 ${failed.length} =====`);
  await browser.close();
  process.exit(failed.length === 0 ? 0 : 1);
} catch (error) {
  console.error('!! UI 验收异常：', error?.message ?? error);
  await page.screenshot({ path: `${OUT}/99-failure.png`, fullPage: true }).catch(() => undefined);
  await browser.close();
  process.exit(2);
}
