#!/usr/bin/env node
/**
 * 使用手册截图抓取（真机 Playwright → 127.0.0.1:5180 vite → :18097 后端，库 cloudpivot_manual2）
 *
 * 用法：MANUAL_BASE=http://127.0.0.1:5180 node scripts/manual/capture.mjs <phase...>
 *   phase ∈ task | flow | purchase | stockin | stockout | inventory | transfer | finance | report
 *
 * 输出：docs/manual/images/<phase>/<nn>-<name>.png
 *      每个 phase 打印 SUMMARY 行（表头 / 行数 / 按钮 / 标签），供手册正文取证。
 *      单个步骤失败只记 !!，不中断该 phase（保证一次跑完 9 个模块）。
 */
import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';
import fs from 'node:fs';
import path from 'node:path';

const BASE = process.env.MANUAL_BASE || 'http://127.0.0.1:5180';
const USER = process.env.MANUAL_USER || 'admin';
const PASS = process.env.MANUAL_PASSWORD || 'Manual@123456';
const SHOT_ROOT = process.env.MANUAL_SHOT_ROOT || '/opt/cloudpivot/docs/manual/images';
// 写库开关：默认只读（不产生脏数据）；MANUAL_WRITE=1 时允许真机建单取证
const WRITE = process.env.MANUAL_WRITE === '1';

const phases = process.argv.slice(2);
if (!phases.length) {
  console.error('用法: node scripts/manual/capture.mjs <phase...>');
  process.exit(2);
}

const loginRes = await fetch(`${BASE}/api/system/auth/login`, {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ username: USER, password: PASS }),
});
const loginBody = await loginRes.json().catch(() => ({}));
if (loginBody.code !== 0 || !loginBody.data?.access_token) {
  console.error('登录失败', loginRes.status, JSON.stringify(loginBody).slice(0, 200));
  process.exit(2);
}
const token = loginBody.data.access_token;
const refresh = loginBody.data.refresh_token;

const browser = await chromium.launch();
const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, locale: 'zh-CN' });
await context.addInitScript(
  ([a, r]) => {
    localStorage.setItem('x_access_token', a);
    localStorage.setItem('x_refresh_token', r);
    localStorage.setItem('x_guide_done', '1');
    localStorage.setItem('cp-onboarding-done', '1');
    localStorage.setItem('cp-guide-dismissed', '1');
  },
  [token, refresh],
);
const page = await context.newPage();
page.setDefaultTimeout(15000);
const httpErrors = [];
page.on('response', (r) => {
  if (r.status() >= 400 && r.url().includes('/api/')) httpErrors.push(`${r.status()} ${r.url().replace(BASE, '').split('?')[0]}`);
});
page.on('pageerror', (e) => httpErrors.push('pageerror: ' + String(e).split('\n')[0].slice(0, 160)));

let shotDir = null;
let shotIdx = 0;
const shots = [];
const notes = [];

function useDir(dir) {
  shotDir = path.join(SHOT_ROOT, dir);
  fs.mkdirSync(shotDir, { recursive: true });
  shotIdx = 0;
}
async function shot(name, opts = {}) {
  shotIdx += 1;
  const file = path.join(shotDir, `${String(shotIdx).padStart(2, '0')}-${name}.png`);
  await page.screenshot({ path: file, fullPage: !!opts.fullPage });
  shots.push(path.relative('/opt/cloudpivot', file));
  console.log(`  [shot] ${path.relative(SHOT_ROOT, file)}`);
  return file;
}
const note = (m) => {
  notes.push(m);
  console.log(`  [note] ${m}`);
};

async function step(label, fn) {
  try {
    await fn();
  } catch (e) {
    note(`!! ${label} 失败: ${String(e).split('\n')[0].slice(0, 180)}`);
    await page.screenshot({ path: path.join(SHOT_ROOT, `_fail-${label.replace(/[^\w-]/g, '_')}.png`) }).catch(() => {});
  }
}

async function dismissOverlays() {
  // 登录后的「你有 N 条待处理」提醒弹窗会拦截所有点击，必须先关
  const unread = page.locator('[data-testid="login-unread-dialog"]');
  if (await unread.isVisible().catch(() => false)) {
    await unread.getByRole('button', { name: '知道了', exact: true }).click({ timeout: 4000 }).catch(() => {});
    await page.waitForTimeout(400);
  }
  for (let i = 0; i < 4; i++) {
    let hit = false;
    for (const sel of ['.t-dialog__close', 'button:has-text("知道了")', '.guide-popup .close']) {
      const el = page.locator(sel).first();
      if (await el.isVisible().catch(() => false)) {
        await el.click({ timeout: 2000 }).catch(() => {});
        hit = true;
        await page.waitForTimeout(250);
        break;
      }
    }
    if (!hit) break;
  }
}

async function goto(routePath, { waitMs = 2200 } = {}) {
  await page.goto(`${BASE}${routePath}`, { waitUntil: 'domcontentloaded' });
  await page.waitForTimeout(waitMs);
  await dismissOverlays();
  await page.waitForTimeout(500);
  await dismissOverlays();
}

async function pageFacts(label) {
  const facts = await page.evaluate(() => {
    const btns = [...document.querySelectorAll('button.t-button')].map((b) => b.innerText.trim()).filter(Boolean);
    const ths = [...document.querySelectorAll('thead th')].map((t) => t.innerText.trim()).filter(Boolean);
    const emptyEl = document.querySelector('.t-table__empty');
    const empty = !!emptyEl;
    const rows = Math.max(0, document.querySelectorAll('tbody tr').length - (empty ? 1 : 0));
    const tabs = [...document.querySelectorAll('.t-tabs__nav-item, .t-radio-button')].map((t) => t.innerText.trim()).filter(Boolean);
    const emptyText = emptyEl ? (emptyEl.innerText || '').trim().slice(0, 30) : '';
    return { title: document.title, btns: [...new Set(btns)].slice(0, 18), ths, rows, tabs: [...new Set(tabs)].slice(0, 10), empty, emptyText };
  });
  note(`${label} | 表头=${JSON.stringify(facts.ths)} 行=${facts.rows} 空=${facts.empty} 按钮=${JSON.stringify(facts.btns)} 标签=${JSON.stringify(facts.tabs)}`);
  return facts;
}

/** 截图保留 toast 提示（提交/保存结果） */
async function showToastShot(name) {
  await page.waitForTimeout(400);
  await shot(name);
  await page.waitForTimeout(1600);
  await dismissOverlays();
}

async function clickBtn(text, { nth = 0 } = {}) {
  const exact = page.getByRole('button', { name: text, exact: true });
  if (await exact.count()) {
    const el = exact.nth(nth);
    await el.scrollIntoViewIfNeeded().catch(() => {});
    await el.click({ timeout: 8000 });
    return true;
  }
  const fuzzy = page.locator(`button:has-text("${text}")`);
  if (await fuzzy.count()) {
    const el = fuzzy.nth(nth);
    await el.scrollIntoViewIfNeeded().catch(() => {});
    await el.click({ timeout: 8000 });
    return true;
  }
  note(`!! 未找到按钮「${text}」`);
  return false;
}

/** 弹出的可见弹窗/抽屉（TDesign） */
const dlg = () => page.locator('.t-dialog:visible, .t-drawer:visible').last();

async function fillIn(scope, selector, value) {
  const box = scope.locator(selector).first();
  await box.waitFor({ state: 'visible', timeout: 8000 });
  await box.click({ timeout: 5000 }).catch(() => {});
  await box.fill(String(value));
  await page.waitForTimeout(200);
}

async function pickOption(root, keyword) {
  await root.scrollIntoViewIfNeeded().catch(() => {});
  await root.click({ timeout: 6000 });
  await page.waitForTimeout(500);
  const opt = page.locator(`.t-select-option:visible:has-text("${keyword}")`).first();
  if (await opt.count()) {
    await opt.click({ timeout: 5000 });
    await page.waitForTimeout(350);
    return true;
  }
  const input = root.locator('input').first();
  if (await input.count()) {
    await input.fill(keyword).catch(() => {});
    await page.waitForTimeout(700);
    const opt2 = page.locator(`.t-select-option:visible:has-text("${keyword}")`).first();
    if (await opt2.count()) {
      await opt2.click({ timeout: 5000 });
      await page.waitForTimeout(350);
      return true;
    }
    await input.press('Enter').catch(() => {});
    await page.waitForTimeout(300);
  }
  note(`!! 下拉未选中「${keyword}」`);
  return false;
}

async function confirmDialog(waitMs = 1800) {
  const d = page.locator('.t-dialog:visible').last();
  let b = d.locator('.t-dialog__footer button.t-button--theme-primary').last();
  if (!(await b.count())) b = d.getByRole('button', { name: /^(确定|确认|创建|保存|提交|确认提交)$/ }).last();
  if (!(await b.count())) b = page.getByRole('button', { name: /^(确定|确认|创建|保存|提交|确认提交)$/ }).last();
  if (!(await b.count())) throw new Error('弹窗内找不到主按钮');
  await b.click({ timeout: 8000 });
  await page.waitForTimeout(waitMs);
  await dismissOverlays();
}

async function toastText() {
  await page.waitForSelector('.t-message', { timeout: 6000 }).catch(() => {});
  const t = page.locator('.t-message').last();
  return ((await t.textContent().catch(() => '')) || '').trim();
}

async function closeDialog() {
  const x = dlg().locator('.t-dialog__close, .t-drawer__close-btn').first();
  if (await x.count()) await x.click().catch(() => {});
  else await page.keyboard.press('Escape').catch(() => {});
  await page.waitForTimeout(600);
}

/** 打开列表第一行的「查看」详情 */
async function openFirstRowDetail() {
  const view = page.locator('tbody tr').first().locator('button:has-text("查看"), a:has-text("查看")').first();
  if (await view.count()) {
    await view.click();
  } else {
    await page.locator('tbody tr .cell-title, tbody tr td').first().click();
  }
  await page.waitForTimeout(1800);
  await dismissOverlays();
}

const PH = {};

// ---------- 1 任务管理 ----------
PH.task = async () => {
  useDir('task');
  await goto('/task/list');
  const f = await pageFacts('任务列表');
  await shot('task-list');

  await step('新建任务弹窗', async () => {
    await clickBtn('新建任务');
    await dlg().waitFor({ state: 'visible', timeout: 10000 });
    await page.waitForTimeout(700);
    await shot('task-create-dialog');
    if (f.rows === 0 || WRITE) {
      await fillIn(page, 'input[placeholder*="一句话说清"]', '【手册样例】10 月采购对账复核');
      await fillIn(page, 'textarea[placeholder*="补充上下文"]', '核对 10 月采购订单与收货单金额，差异列入周会。');
      await shot('task-create-filled');
      await confirmDialog(2000);
      note(`新建任务返回：${(await toastText()) || '(无 toast)'}`);
      await page.waitForTimeout(1500);
    } else {
      // 库里有数据时只截图表单，不落库，保证重跑不产生脏数据
      await closeDialog();
    }
  });
  await pageFacts('任务列表(创建后)');

  await step('任务详情', async () => {
    await dismissOverlays();
    await page.keyboard.press('Escape').catch(() => {});
    await page.waitForTimeout(500);
    const view = page.locator('tbody tr').first().locator('button:has-text("查看")').first();
    if (await view.count()) await view.click({ timeout: 8000 });
    else await page.locator('tbody tr td').first().click({ timeout: 8000, force: true });
    await page.waitForTimeout(1800);
    await dismissOverlays();
    await pageFacts('任务详情');
    await shot('task-detail');
    await closeDialog();
  });

  for (const [p, label, name] of [
    ['/task/board', '任务看板', 'task-board'],
    ['/task/gantt', '甘特图', 'task-gantt'],
    ['/task/templates', '任务模板', 'task-templates'],
  ]) {
    await step(label, async () => {
      await goto(p);
      await pageFacts(label);
      await shot(name);
    });
  }
};

// ---------- 2 审批流 ----------
PH.flow = async () => {
  useDir('flow');
  await goto('/flow/def');
  await pageFacts('流程定义');
  await shot('flow-def-list');

  await step('流程编辑器', async () => {
    const edit = page.locator('tbody tr').first().locator('button:has-text("设计"), button:has-text("编辑"), a:has-text("编辑"), a:has-text("设计")').first();
    if (!(await edit.count())) throw new Error('列表无设计/编辑入口');
    await edit.click();
    await page.waitForTimeout(2800);
    await dismissOverlays();
    await pageFacts('流程编辑器');
    await shot('flow-editor');
  });

  for (const [p, label, name] of [
    ['/flow/templates', '流程模板库', 'flow-templates'],
    ['/flow/task', '审批中心', 'flow-task-center'],
    ['/flow/start', '发起审批', 'flow-start'],
  ]) {
    await step(label, async () => {
      await goto(p);
      await pageFacts(label);
      await shot(name);
    });
  }

  await step('审批单详情', async () => {
    await goto('/flow/task');
    const link = page.getByText(/IN-\d{8}-\d+/).first();
    if (!(await link.count())) throw new Error('待我审批无卡片');
    await link.click();
    await page.waitForTimeout(2000);
    await dismissOverlays();
    await pageFacts('审批单详情');
    await shot('flow-task-detail');
    await closeDialog();
  });

  await step('我已审批页签', async () => {
    await goto('/flow/task');
    await page.getByText('我发起的', { exact: true }).first().click();
    await page.waitForTimeout(1500);
    await pageFacts('我发起的');
    await shot('flow-task-mine');
  });
};

// ---------- 3 采购订单 ----------
PH.purchase = async () => {
  useDir('purchase');
  await goto('/business/erp/purchase-order');
  await pageFacts('采购订单列表');
  await shot('purchase-list');

  await step('新建采购单', async () => {
    await clickBtn('新建采购单');
    await dlg().waitFor({ state: 'visible', timeout: 10000 });
    await page.waitForTimeout(900);
    await shot('purchase-create-form');
    const spot = dlg().locator('.t-radio-button:has-text("现货采购")').first();
    if (await spot.count()) {
      await spot.click();
      await page.waitForTimeout(500);
      await shot('purchase-create-spot');
    }
    await closeDialog();
  });

  await step('采购单详情', async () => {
    await openFirstRowDetail();
    await pageFacts('采购单详情');
    await shot('purchase-detail');
    await closeDialog();
  });

  await step('提交审批', async () => {
    // 优先挑「既有明细、又能提交」的单据；已提交的行不再有该按钮
    let row = page.locator('tbody tr:has-text("P1FIX-PO-1"):has-text("提交审批")').first();
    if (!(await row.count())) row = page.locator('tbody tr:has-text("提交审批")').first();
    if (!(await row.count())) {
      note('列表已无「提交审批」可点的单据（全部已提交），跳过——不是失败');
      return;
    }
    const no = (await row.locator('td').first().innerText()).trim();
    await row.getByText('提交审批', { exact: true }).first().click();
    await page.waitForTimeout(1200);
    await shot('purchase-submit-confirm');
    await confirmDialog(2500);
    note(`提交审批（${no}）返回：${(await toastText()) || '(无 toast)'}`);
    await showToastShot('purchase-after-submit');
  });
};

// ---------- 4 收货入库 ----------
PH.stockin = async () => {
  useDir('stockin');
  await goto('/business/erp/stock-in');
  await pageFacts('收货入库列表');
  await shot('stockin-list');

  await step('收货单详情', async () => {
    await openFirstRowDetail();
    await pageFacts('收货单详情');
    await shot('stockin-detail');
    await closeDialog();
  });

  await step('新建收货单', async () => {
    const btn = page.locator('button:has-text("新建收货"), button:has-text("从采购单生成"), button:has-text("生成收货")').first();
    if (!(await btn.count())) throw new Error('无新建入口');
    await btn.click();
    await page.waitForTimeout(1500);
    await dismissOverlays();
    if (await dlg().isVisible().catch(() => false)) {
      await page.waitForTimeout(600);
    }
    await shot('stockin-create-form');
    await closeDialog();
  });
};

// ---------- 5 出库单 ----------
PH.stockout = async () => {
  useDir('stockout');
  await goto('/business/erp/stock-out');
  const f = await pageFacts('出库单列表');
  await shot('stockout-list');

  await step('新建出库单', async () => {
    const btn = page.locator('button:has-text("新建出库"), button:has-text("新建")').first();
    if (!(await btn.count())) throw new Error('无新建入口');
    await btn.click();
    await dlg().waitFor({ state: 'visible', timeout: 10000 });
    await page.waitForTimeout(900);
    await shot('stockout-create-form');
    await closeDialog();
  });

  await step('出库单详情', async () => {
    await openFirstRowDetail();
    await pageFacts('出库单详情');
    await shot('stockout-detail');
    await closeDialog();
  });
  note(`出库单现有行数=${f.rows}`);
};

// ---------- 5b 出库单：真机建单（演示用，仅空库时执行） ----------
PH.stockout_create = async () => {
  useDir('stockout-create');
  await goto('/business/erp/stock-out');
  const f = await pageFacts('出库单列表(建单前)');
  if (!f.empty) {
    note(`出库单已有数据，跳过建单`);
    return;
  }
  await clickBtn('新建出库单');
  await dlg().waitFor({ state: 'visible', timeout: 10000 });
  await page.waitForTimeout(900);
  const root = page;
  await root.getByText('领用', { exact: true }).first().click().catch(() => {});
  await page.waitForTimeout(300);
  await shot('stockout-form-type');
  // 出库仓库：点 select 外框展开 → 选第一个可见项
  await root.locator('.t-select:has(input[placeholder*="选择出库仓库"])').first().click({ force: true });
  await page.waitForTimeout(1000);
  const whOpt = page.locator('.t-select-option:visible').first();
  const whName = ((await whOpt.innerText().catch(() => '')) || '').trim();
  await whOpt.click();
  await page.waitForTimeout(400);
  note(`出库仓库选择：${whName}`);
  await shot('stockout-form-warehouse');
  // 物料：点 select 外框展开 → 选第一个可见项
  await root.locator('.t-select:has(input[placeholder*="搜索物料编码"])').first().click({ force: true });
  await page.waitForTimeout(1200);
  const matOpt = page.locator('.t-select-option:visible').first();
  const matName = ((await matOpt.innerText().catch(() => '')) || '').trim();
  await matOpt.click();
  await page.waitForTimeout(500);
  note(`物料选择：${matName}`);
  const qty = root.locator('input[placeholder="数量"]').first();
  await qty.click();
  await qty.fill('10');
  await qty.press('Tab');
  await page.waitForTimeout(500);
  await shot('stockout-form-filled');
  await clickBtn('保存草稿');
  await page.waitForTimeout(2000);
  note(`保存出库单返回：${(await toastText()) || '(无 toast)'}`);
  await dismissOverlays();
  await page.waitForTimeout(1200);
  await pageFacts('出库单列表(建单后)');
  await shot('stockout-list-after-create');
};

// ---------- 6 库存 ----------
PH.inventory = async () => {
  useDir('inventory');
  for (const [p, label, name] of [
    ['/business/erp/inventory', '即时库存', 'inventory-stock'],
    ['/business/erp/inventory-location', '库位库存', 'inventory-location'],
    ['/business/erp/inventory-txn', '库存流水', 'inventory-txn'],
  ]) {
    await step(label, async () => {
      await goto(p);
      await pageFacts(label);
      await shot(name);
    });
  }
};

// ---------- 7 调拨 ----------
PH.transfer = async () => {
  useDir('transfer');
  await goto('/business/erp/transfer');
  await pageFacts('调拨单列表');
  await shot('transfer-list');

  await step('调拨单详情', async () => {
    await openFirstRowDetail();
    await pageFacts('调拨单详情');
    await shot('transfer-detail');
    await closeDialog();
  });

  await step('新建调拨单', async () => {
    await clickBtn('新建调拨单');
    await dlg().waitFor({ state: 'visible', timeout: 10000 });
    await page.waitForTimeout(900);
    await shot('transfer-create-form');
    await closeDialog();
  });
};

// ---------- 8 财务 ----------
PH.finance = async () => {
  useDir('finance');
  for (const [p, label, name] of [
    ['/business/erp/payable', '应付单', 'finance-payable'],
    ['/business/erp/voucher', '凭证', 'finance-voucher'],
    ['/business/erp/ledger', '总账/明细账', 'finance-ledger'],
    ['/business/erp/period', '会计期间', 'finance-period'],
  ]) {
    await step(label, async () => {
      await goto(p);
      await pageFacts(label);
      await shot(name);
    });
  }
  await step('应付单详情', async () => {
    await goto('/business/erp/payable');
    await openFirstRowDetail();
    await pageFacts('应付单详情');
    await shot('finance-payable-detail');
    await closeDialog();
  });
};

// ---------- 9 报表 ----------
PH.report = async () => {
  useDir('report');
  for (const [p, label, name] of [
    ['/business/erp/report/balance-sheet', '资产负债表', 'report-balance-sheet'],
    ['/business/erp/report/income-statement', '利润表', 'report-income-statement'],
    ['/business/erp/report/cash-flow', '现金流量表', 'report-cash-flow'],
  ]) {
    await step(label, async () => {
      await goto(p, { waitMs: 2500 });
      await pageFacts(label);
      await shot(name);
    });
  }
};

for (const p of phases) {
  if (!PH[p]) {
    console.error(`未知 phase: ${p}`);
    continue;
  }
  console.log(`\n===== phase ${p} =====`);
  await step(`phase-${p}`, async () => {
    await PH[p]();
  });
}

if (httpErrors.length) {
  console.log('\n--- 4xx/5xx 与页面异常 ---');
  console.log([...new Set(httpErrors)].slice(0, 25).join('\n'));
} else {
  console.log('\n--- 无 4xx/5xx、无 pageerror ---');
}
console.log(`\n共 ${shots.length} 张截图`);
await browser.close();
