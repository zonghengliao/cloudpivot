#!/usr/bin/env node
/**
 * 使用手册（二）截图抓取：财务 8 个 + 平台 5 个功能（P2-2B）
 *
 * 用法：MANUAL_BASE=http://127.0.0.1:5180 node scripts/manual/capture2.mjs <phase...>
 *   phase ∈ voucher payable payment ledger period yearend aux budget sys perm notice attach inspect
 *          seed_payment seed_attach（一次性写库种子，跑手册前只跑一次）
 *
 * 输出：docs/manual/images/p2b/<phase>/<nn>-<name>.png
 *      每个 phase 打印 SUMMARY 行（表头 / 行数 / 按钮 / 标签），供手册正文取证。
 *      单步失败只记 !!，不中断整批。
 *
 * 与 capture.mjs 的分工：capture.mjs 服务手册（一）9 个核心业务；本脚本不碰它的目录与逻辑，
 * 失败互不影响。默认只读；MANUAL_WRITE=1 才允许 seed_* 真机写库取证。
 */
import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';
import fs from 'node:fs';
import path from 'node:path';

const BASE = process.env.MANUAL_BASE || 'http://127.0.0.1:5180';
const API = process.env.MANUAL_API || 'http://127.0.0.1:18097';
const USER = process.env.MANUAL_USER || 'admin';
const PASS = process.env.MANUAL_PASSWORD || 'Manual@123456';
const SHOT_ROOT = process.env.MANUAL_SHOT_ROOT || '/opt/cloudpivot/docs/manual/images/p2b';
const WRITE = process.env.MANUAL_WRITE === '1';
const VERIFY_PAYABLE = process.env.MANUAL_PAYABLE || 'AP-20261102-000827';

const phases = process.argv.slice(2);
if (!phases.length) {
  console.error('用法: node scripts/manual/capture2.mjs <phase...>');
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
/** 元素级截图（付款记录时间线 / 附件面板这类长区块） */
async function elShot(locator, name, opts = {}) {
  shotIdx += 1;
  const file = path.join(shotDir, `${String(shotIdx).padStart(2, '0')}-${name}.png`);
  const el = locator.first();
  await el.scrollIntoViewIfNeeded().catch(() => {});
  await el.screenshot({ path: file, ...opts }).catch(async () => {
    await page.screenshot({ path: file, fullPage: true });
  });
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

async function gotoAbs(url, { waitMs = 1200 } = {}) {
  await page.goto(url, { waitUntil: 'domcontentloaded' });
  await page.waitForTimeout(waitMs);
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

const dlg = () => page.locator('.t-dialog:visible, .t-drawer:visible').last();

async function closeDialog() {
  const d = page.locator('.t-dialog:visible').last();
  const x = d.locator('.t-dialog__close').first();
  if (await x.count()) await x.click({ timeout: 3000 }).catch(() => {});
  else await page.keyboard.press('Escape').catch(() => {});
  await page.waitForTimeout(600);
}

async function confirmDialog(waitMs = 1800) {
  const d = page.locator('.t-dialog:visible').last();
  let b = d.locator('.t-dialog__footer button.t-button--theme-primary').last();
  if (!(await b.count())) b = d.getByRole('button', { name: /^(确定|确认|创建|保存|提交|确认提交|确认付款)$/ }).last();
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

/** 打开 TDesign 下拉并选一条（keyword 空 = 选第一条可见项） */
async function pickSelect(root, keyword) {
  await root.scrollIntoViewIfNeeded().catch(() => {});
  await root.click({ timeout: 6000 });
  await page.waitForTimeout(600);
  const opt = keyword
    ? page.locator(`.t-select-option:visible:has-text("${keyword}")`).first()
    : page.locator('.t-select-option:visible').first();
  if (!(await opt.count())) {
    note(`!! 下拉无选项「${keyword || '(首项)'}」`);
    return '';
  }
  const text = ((await opt.innerText().catch(() => '')) || '').trim();
  await opt.click({ timeout: 5000 });
  await page.waitForTimeout(400);
  return text;
}

/** 打开列表第一行的「查看」详情 */
async function openFirstRowDetail() {
  const view = page.locator('tbody tr').first().locator('button:has-text("查看"), a:has-text("查看")').first();
  if (await view.count()) await view.click();
  else await page.locator('tbody tr td').first().click();
  await page.waitForTimeout(1800);
  await dismissOverlays();
}

const PH = {};

// ---------- 10 凭证 ----------
PH.voucher = async () => {
  useDir('voucher');
  await goto('/business/erp/voucher');
  await pageFacts('凭证列表');
  await shot('voucher-list');

  await step('凭证筛选区', async () => {
    await page.locator('[data-testid="voucher-filter-period"]').click();
    await page.waitForTimeout(400);
    await page.keyboard.press('Escape');
    await page.locator('[data-testid="voucher-filter-status"]').click();
    await page.waitForTimeout(600);
    const opts = await page.locator('.t-select-option:visible').allInnerTexts();
    note(`状态筛选项=${JSON.stringify(opts.map((s) => s.trim()))}`);
    await page.keyboard.press('Escape');
    await shot('voucher-filters');
  });

  await step('新增凭证（分录编辑器 + 借贷平衡）', async () => {
    await page.locator('[data-testid="btn-voucher-create"]').click();
    await page.locator('[data-testid="voucher-create-dialog"]').waitFor({ state: 'visible', timeout: 10000 });
    await page.waitForTimeout(700);
    await shot('voucher-create-empty');

    await page.locator('[data-testid="voucher-create-summary"] input').fill('【手册样例】计提本月折旧');
    const accs = page.locator('[data-testid^="voucher-create-account-"]');
    const name1 = await pickSelect(accs.nth(0), '累计折旧');
    note(`分录1科目=${name1}`);
    const debit1 = page.locator('[data-testid^="voucher-create-debit-"] input').first();
    await debit1.click();
    await debit1.fill('1000');
    await debit1.press('Tab');
    const name2 = await pickSelect(accs.nth(1), '管理费用');
    note(`分录2科目=${name2}`);
    const credit2 = page.locator('[data-testid^="voucher-create-credit-"] input').last();
    await credit2.click();
    await credit2.fill('1000');
    await credit2.press('Tab');
    await page.waitForTimeout(500);
    const balance = ((await page.locator('[data-testid="voucher-create-balance"]').innerText().catch(() => '')) || '').trim();
    note(`借贷校验=${balance}`);
    await shot('voucher-create-balanced');
    await closeDialog();
  });

  await step('凭证详情（分录 + 合计 + 借贷平衡）', async () => {
    const view = page.locator('[data-testid^="btn-voucher-view-"]').first();
    if (!(await view.count())) throw new Error('列表无「查看」入口');
    await view.click();
    await page.locator('[data-testid="voucher-drawer"]').waitFor({ state: 'visible', timeout: 10000 });
    await page.waitForTimeout(1200);
    const title = ((await page.locator('[data-testid="voucher-detail-title"]').innerText().catch(() => '')) || '').trim();
    const rows = await page.locator('[data-testid="voucher-entry-table"] tbody tr').count();
    note(`凭证明细=${title} 分录行=${rows}`);
    await pageFacts('凭证详情');
    await shot('voucher-detail');
    await page.locator('[data-testid="btn-voucher-drawer-close"]').click({ timeout: 4000 }).catch(() => {});
    await page.waitForTimeout(600);
  });
};

// ---------- 11 应付 ----------
PH.payable = async () => {
  useDir('payable');
  await goto('/business/erp/payable');
  await pageFacts('应付单列表');
  await shot('payable-list');

  await step('应付单详情（动作条）', async () => {
    await openFirstRowDetail();
    await page.waitForTimeout(800);
    await pageFacts('应付单详情');
    await shot('payable-detail');
  });

  await step('应付作废弹窗（原因必填）', async () => {
    const btn = page.locator('.t-drawer:visible').getByRole('button', { name: /^作废/ }).first();
    if (!(await btn.count())) throw new Error('详情动作条无「作废」');
    await btn.click({ timeout: 6000 });
    await page.locator('[data-testid="payable-cancel-dialog"]').waitFor({ state: 'visible', timeout: 8000 });
    await page.waitForTimeout(600);
    await shot('payable-cancel-dialog');
    await closeDialog();
    await page.locator('[data-testid="btn-payable-drawer-close"]').click({ timeout: 4000 }).catch(() => {});
    await page.waitForTimeout(500);
  });
};

// ---------- 12 付款 ----------
async function openPayableByNo(no) {
  await goto('/business/erp/payable');
  const row = page.locator(`tbody tr:has-text("${no}")`).first();
  if (!(await row.count())) throw new Error(`列表找不到 ${no}`);
  await row.getByText('查看', { exact: true }).first().click();
  await page.waitForTimeout(2000);
  await dismissOverlays();
}

PH.payment = async () => {
  useDir('payment');
  await goto('/business/erp/payable');
  await pageFacts('应付单列表（付款入口）');

  await step('付款弹窗（登记付款）', async () => {
    await openPayableByNo(VERIFY_PAYABLE);
    const payBtn = page.locator('.t-drawer:visible').getByRole('button', { name: /发起付款/ }).first();
    if (!(await payBtn.count())) throw new Error('详情无「发起付款」按钮');
    await payBtn.click({ timeout: 6000 });
    await page.locator('[data-testid="payable-pay-dialog"]').waitFor({ state: 'visible', timeout: 8000 });
    await page.waitForTimeout(700);
    const amount = await page.locator('[data-testid="payable-pay-amount"] input').inputValue().catch(() => '');
    const method = await page.locator('[data-testid="payable-pay-method"] .t-input__inner').inputValue().catch(() => '');
    note(`付款弹窗预填金额=${amount || '(空)'} 付款方式=${method || '(空)'}`);
    await shot('payment-dialog');
    if (WRITE) {
      const amt = page.locator('[data-testid="payable-pay-amount"] input');
      await amt.click();
      await amt.fill('30');
      await amt.press('Tab');
      await pickSelect(page.locator('[data-testid="payable-pay-method"]'), '银行转账');
      await page.waitForTimeout(300);
      await shot('payment-dialog-filled');
      await confirmDialog(2600);
      note(`登记付款返回：${(await toastText()) || '(无 toast)'}`);
      await page.waitForTimeout(2000);
    } else {
      await closeDialog();
    }
  });

  await step('付款记录与审批进度', async () => {
    await openPayableByNo(VERIFY_PAYABLE);
    const section = page.locator('[data-testid="payable-payments"]').first();
    if (!(await section.count())) throw new Error('详情无付款记录区块');
    const text = ((await section.innerText()) || '').replace(/\s+/g, ' ').slice(0, 260);
    note(`付款记录：${text}`);
    await elShot(section, 'payment-records');
    const progress = page.locator('[data-testid="payable-approval-progress"]').first();
    if (await progress.count()) {
      const ptext = ((await progress.innerText()) || '').replace(/\s+/g, ' ').slice(0, 200);
      note(`付款审批进度：${ptext}`);
      await elShot(progress, 'payment-approval-progress');
    } else note('详情无审批进度区块（未配流程或已归档）');
    await page.locator('[data-testid="btn-payable-drawer-close"]').click({ timeout: 4000 }).catch(() => {});
    await page.waitForTimeout(500);
  });
};

// ---------- 13 总账 ----------
PH.ledger = async () => {
  useDir('ledger');
  await goto('/business/erp/ledger');
  await pageFacts('总账（流水）初始');
  await shot('ledger-initial');

  await step('按科目查流水（逐行余额）', async () => {
    const picked = await pickSelect(page.locator('[data-testid="ledger-filter-account"]'), '应付账款');
    note(`科目选择=${picked}`);
    await page.locator('[data-testid="btn-ledger-search"]').click();
    await page.waitForTimeout(2500);
    const facts = await pageFacts('总账流水结果');
    const verdict = ((await page.locator('[data-testid="ledger-footer-verdict"]').innerText().catch(() => '')) || '').trim();
    note(`流水合计校验=${verdict}`);
    await shot('ledger-flows');
    if (facts.rows === 0) note('!! 该科目无流水，换科目重试');
  });

  await step('科目余额表（2026-10）', async () => {
    await page.getByText('科目余额表', { exact: true }).first().click();
    await page.waitForTimeout(1200);
    await page.locator('[data-testid="trial-filter-period"] input').fill('2026-10');
    await page.locator('[data-testid="btn-trial-search"]').click();
    await page.waitForTimeout(2500);
    await pageFacts('科目余额表');
    const verdict = ((await page.locator('[data-testid="trial-footer-verdict"]').innerText().catch(() => '')) || '').trim();
    note(`余额表试算结论=${verdict}`);
    await shot('ledger-trial-balance');
  });
};

// ---------- 14 期间 ----------
PH.period = async () => {
  useDir('period');
  await goto('/business/erp/period');
  await pageFacts('会计期间列表');
  await shot('period-list');

  await step('开账弹窗', async () => {
    await page.locator('[data-testid="btn-period-open"]').click();
    await page.locator('[data-testid="period-open-dialog"]').waitFor({ state: 'visible', timeout: 8000 });
    await page.waitForTimeout(700);
    await shot('period-open-dialog');
    await closeDialog();
  });

  await step('结账预检（阻断项 + 巡检提醒）', async () => {
    const row = page.locator('tbody tr:has-text("2026-10")').first();
    let btn = row.locator('[data-testid^="btn-period-close-"]').first();
    if (!(await btn.count())) btn = page.locator('[data-testid^="btn-period-close-"]').first();
    const code = ((await btn.getAttribute('data-testid')) || '').replace('btn-period-close-', '');
    note(`结账目标期间=${code}`);
    await btn.click();
    await page.locator('[data-testid="period-close-dialog"]').waitFor({ state: 'visible', timeout: 10000 });
    await page.waitForTimeout(3000);
    const body = ((await page.locator('[data-testid="period-close-dialog"]').innerText().catch(() => '')) || '').replace(/\s+/g, ' ');
    note(`结账预检：${body.slice(0, 300)}`);
    await shot('period-close-check', { fullPage: true });
    await closeDialog();
  });

  await step('反结账弹窗（原因必填）', async () => {
    await goto('/business/erp/period');
    await pickSelect(page.locator('[data-testid="period-filter-status"]'), '已结账');
    await page.locator('[data-testid="btn-period-search"]').click();
    await page.waitForTimeout(2000);
    const btn = page.locator('[data-testid^="btn-period-reopen-"]').first();
    if (!(await btn.count())) throw new Error('无「反结账」按钮');
    const code = ((await page.locator('tbody tr').first().locator('td').first().innerText().catch(() => '')) || '').trim();
    note(`反结账目标期间=${code}`);
    await btn.click();
    await page.locator('[data-testid="period-reopen-dialog"]').waitFor({ state: 'visible', timeout: 8000 });
    await page.waitForTimeout(700);
    await shot('period-reopen-dialog');
    await closeDialog();
  });
};

// ---------- 15 年结 ----------
PH.yearend = async () => {
  useDir('yearend');
  await goto('/business/erp/year-end', { waitMs: 3000 });
  await pageFacts('年度结转页');
  const tag = ((await page.locator('[data-testid="yearend-status-tag"]').innerText().catch(() => '')) || '').trim();
  const periods = ((await page.locator('[data-testid="yearend-periods-closed"]').innerText().catch(() => '')) || '').trim();
  note(`年结状态=${tag} 期间=${periods}`);
  await shot('yearend-page', { fullPage: true });

  await step('年度结转弹窗', async () => {
    const btn = page.locator('[data-testid="btn-yearend-close"]').first();
    if (!(await btn.count())) throw new Error('无「年度结转」按钮');
    const disabled = await btn.isDisabled().catch(() => false);
    note(`年度结转按钮 disabled=${disabled}`);
    if (!disabled) {
      await btn.click();
      await page.locator('[data-testid="yearend-close-dialog"]').waitFor({ state: 'visible', timeout: 8000 });
      await page.waitForTimeout(800);
      await shot('yearend-close-dialog');
      await closeDialog();
    } else {
      const blocked = ((await page.locator('[data-testid="yearend-close-blocked"]').innerText().catch(() => '')) || '').trim();
      note(`年结被拦原因=${blocked}`);
    }
  });

  await step('切换年度看前置检查', async () => {
    const s = page.locator('[data-testid="yearend-year-select"]');
    await pickSelect(s, '2026 年');
    await page.waitForTimeout(2500);
    const lines = await page.locator('[data-testid="yearend-check-list"] li').allInnerTexts().catch(() => []);
    note(`前置检查=${JSON.stringify(lines.map((t) => t.replace(/\s+/g, ' ').trim()).slice(0, 8))}`);
    await shot('yearend-check-2026', { fullPage: true });
  });
};

// ---------- 16 辅助核算 ----------
PH.aux = async () => {
  useDir('aux');
  await goto('/business/erp/account');
  await pageFacts('会计科目（辅助核算维度的挂载点）');
  await shot('aux-account-tree');

  await step('凭证分录里的辅助核算', async () => {
    await goto('/business/erp/voucher');
    await page.locator('[data-testid="voucher-filter-no"] input').fill('PZ-20261102-001578');
    await page.locator('[data-testid="btn-voucher-search"]').click();
    await page.waitForTimeout(2000);
    const view = page.locator('[data-testid^="btn-voucher-view-"]').first();
    if (!(await view.count())) throw new Error('按凭证号无结果');
    await view.click();
    await page.locator('[data-testid="voucher-drawer"]').waitFor({ state: 'visible', timeout: 10000 });
    await page.waitForTimeout(1200);
    await shot('aux-voucher-entries');
    await page.locator('[data-testid="btn-voucher-drawer-close"]').click({ timeout: 4000 }).catch(() => {});
  });

  await step('总账页（辅助核算维度筛选未在前端暴露）', async () => {
    await goto('/business/erp/ledger');
    // 只记录口径，不再截图：该页与手册 13 的截图完全一致（重复图会被去重检查抓出），
    // 结论在本条 note 里，正文引用 13 的截图说明「只有科目 + 期间，没有维度筛选」。
    await pageFacts('总账页（无辅助核算维度筛选）');
    note('总账页筛选只有 科目 / 起始 / 结束，无辅助核算维度入口（见 13-总账 的截图）');
  });
};

// ---------- 17 预算（待做） ----------
PH.budget = async () => {
  useDir('budget');
  await goto('/business/erp/budget');
  await pageFacts('预算页（未实现）');
  await shot('budget-404');

  await step('财务目录菜单（无预算项）', async () => {
    await goto('/dashboard');
    const menu = page.locator('aside').getByText('财务管理', { exact: true }).first();
    if (await menu.count()) {
      await menu.click({ timeout: 6000 }).catch(() => {});
      await page.waitForTimeout(900);
    }
    await shot('budget-menu');
  });
};

// ---------- 18 系统管理 ----------
PH.sys = async () => {
  useDir('sys');
  await goto('/system/user');
  await pageFacts('用户管理');
  await shot('sys-user-list');

  await step('新增用户弹窗', async () => {
    await clickBtn('新增用户');
    await dlg().waitFor({ state: 'visible', timeout: 8000 });
    await page.waitForTimeout(700);
    await shot('sys-user-create');
    await closeDialog();
  });

  await step('角色管理', async () => {
    await goto('/system/role');
    await pageFacts('角色管理');
    const first = page.locator('.role-item').first();
    if (await first.count()) {
      await first.click();
      await page.waitForTimeout(1500);
    }
    await shot('sys-role-list');
  });

  await step('菜单管理', async () => {
    await goto('/system/menu');
    await pageFacts('菜单管理');
    await shot('sys-menu-tree');
  });

  await step('部门管理（空库 + 新增弹窗）', async () => {
    await goto('/system/dept');
    await pageFacts('部门管理');
    await shot('sys-dept-empty');
    await clickBtn('新增部门');
    await dlg().waitFor({ state: 'visible', timeout: 8000 });
    await page.waitForTimeout(700);
    await shot('sys-dept-create');
    await closeDialog();
  });
};

// ---------- 19 权限 ----------
PH.perm = async () => {
  useDir('perm');
  await goto('/system/role');
  await pageFacts('角色管理（权限分配入口）');
  await step('角色权限树', async () => {
    // 优先用「财务经理」（有教学意义的实名角色）；与手册 18 的角色页（第一个角色）不重复，避免同图重复
    const prefer = page.locator('.role-item:has-text("财务经理")').first();
    const roles = page.locator('.role-item');
    if (await prefer.count()) {
      await prefer.click();
    } else if (await roles.count()) {
      await roles.nth((await roles.count()) > 1 ? 1 : 0).click();
    }
    await page.waitForTimeout(1800);
    const boxes = await page.locator('.t-checkbox').count();
    const roleName = ((await page.locator('.role-item.is-active, .role-item.active').first().innerText().catch(() => '')) || '').trim().slice(0, 40);
    note(`权限树角色=${roleName || '(未知)'} 复选框数=${boxes}`);
    await shot('perm-role-tree', { fullPage: true });
  });

  await step('数据权限', async () => {
    await goto('/system/data-scope');
    await pageFacts('数据权限');
    await shot('perm-data-scope');
    const cfg = page.locator('tbody a:has-text("配置"), tbody a:has-text("查看")').first();
    if (await cfg.count()) {
      await cfg.click({ timeout: 6000 });
      await page.waitForTimeout(1800);
      const drawer = page.locator('.t-drawer:visible').last();
      if (await drawer.count()) {
        const text = ((await drawer.innerText().catch(() => '')) || '').replace(/\s+/g, ' ').slice(0, 200);
        note(`数据权限抽屉=${text}`);
        await shot('perm-data-scope-config');
        await page.keyboard.press('Escape');
        await page.waitForTimeout(600);
      }
    } else note('列表无「配置 / 查看」入口');
  });

  await step('角色互斥', async () => {
    await goto('/system/role-mutex');
    await pageFacts('角色互斥');
    await shot('perm-role-mutex');
    await clickBtn('新增互斥规则');
    await dlg().waitFor({ state: 'visible', timeout: 8000 });
    await page.waitForTimeout(700);
    await shot('perm-role-mutex-create');
    await closeDialog();
  });

  await step('字段脱敏', async () => {
    await goto('/system/field-mask');
    await pageFacts('字段脱敏');
    await shot('perm-field-mask');
    await clickBtn("新增规则");
    await dlg().waitFor({ state: 'visible', timeout: 8000 });
    await page.waitForTimeout(700);
    await shot('perm-field-mask-create');
    await closeDialog();
  });
};

// ---------- 20 通知 ----------
PH.notice = async () => {
  useDir('notice');
  await goto('/dashboard');
  await step('顶栏铃铛', async () => {
    const bell = page.locator('.notice-bell__trigger').first();
    if (!(await bell.count())) throw new Error('顶栏无铃铛');
    await bell.click({ timeout: 6000 });
    await page.waitForTimeout(1200);
    const panel = page.locator('.notice-bell__panel').first();
    const text = ((await panel.innerText().catch(() => '')) || '').replace(/\s+/g, ' ').slice(0, 160);
    note(`铃铛面板=${text}`);
    await shot('notice-bell');
    await page.keyboard.press('Escape');
  });

  await step('消息中心', async () => {
    await goto('/business/notice/message');
    await pageFacts('消息中心');
    await shot('notice-center');
  });

  await step('通知模板', async () => {
    await goto('/business/notice/template');
    await pageFacts('通知模板');
    await shot('notice-template-list');
    await clickBtn('新增模板');
    await dlg().waitFor({ state: 'visible', timeout: 8000 });
    await page.waitForTimeout(800);
    await shot('notice-template-create');
    await closeDialog();
  });

  await step('发送通知', async () => {
    await goto('/business/notice/send');
    await pageFacts('发送通知');
    await shot('notice-send-form');
  });
};

// ---------- 21 附件 ----------
PH.attach = async () => {
  useDir('attach');
  await goto('/business/storage/file');
  await pageFacts('文件管理（对象存储）');
  await shot('attach-file-list');

  await step('单据附件面板', async () => {
    await openPayableByNo(VERIFY_PAYABLE);
    const panel = page.locator('[data-testid="payable-attach-drop"]').first();
    const panelRoot = page.locator('.doc-attach').first();
    await panelRoot.scrollIntoViewIfNeeded().catch(() => {});
    await page.waitForTimeout(800);
    const text = ((await panelRoot.innerText().catch(() => '')) || '').replace(/\s+/g, ' ').slice(0, 160);
    note(`附件面板=${text}`);
    await elShot(panelRoot, 'attach-doc-panel');
    if (!(await panel.count())) note('!! 附件面板无拖拽区（可能单未保存）');
    await page.locator('[data-testid="btn-payable-drawer-close"]').click({ timeout: 4000 }).catch(() => {});
  });

  await step('文件预览抽屉', async () => {
    await goto('/business/storage/file');
    const nameLink = page.locator('tbody .cell-file').first();
    if (await nameLink.count()) {
      await nameLink.click({ timeout: 6000 });
      await page.waitForTimeout(2200);
      const drawer = page.locator('.t-drawer:visible').last();
      const head = ((await drawer.innerText().catch(() => '')) || '').replace(/\s+/g, ' ').slice(0, 120);
      note(`预览抽屉=${head}`);
      await shot('attach-preview');
      await page.keyboard.press('Escape');
    } else note('文件列表为空（尚未上传种子文件）');
  });
};

// ---------- 22 巡检 ----------
PH.inspect = async () => {
  useDir('inspect');
  await goto('/business/erp/period');
  await step('结账预检里的全库巡检提醒', async () => {
    const row = page.locator('tbody tr:has-text("2026-10")').first();
    let closeBtn = row.locator('[data-testid^="btn-period-close-"]').first();
    if (!(await closeBtn.count())) closeBtn = page.locator('[data-testid^="btn-period-close-"]').first();
    await closeBtn.click();
    await page.locator('[data-testid="period-close-dialog"]').waitFor({ state: 'visible', timeout: 10000 });
    await page.waitForTimeout(3000);
    const warn = page.locator('[data-testid="period-close-warnings"]').first();
    const text = ((await warn.innerText().catch(() => '')) || '').replace(/\s+/g, ' ');
    note(`巡检提醒=${text.slice(0, 220) || '(无提醒)'}`);
    if (await warn.count()) await elShot(warn, 'inspect-warnings');
    else await shot('inspect-close-check');
    await closeDialog();
  });

  await step('库存页（巡检链接落点）', async () => {
    await goto('/business/erp/inventory?audit=1');
    await pageFacts('即时库存（?audit=1）');
    await shot('inspect-inventory');
  });

  await step('健康检查 /health', async () => {
    await gotoAbs(`${API}/health`);
    const text = await page.locator('body').innerText().catch(() => '');
    note(`/health=${text.replace(/\s+/g, ' ').slice(0, 200)}`);
    await shot('inspect-health');
  });
};

// ---------- 种子：一笔部分付款（一次性写库） ----------
PH.seed_payment = async () => {
  useDir('seed-payment');
  if (!WRITE) {
    note('MANUAL_WRITE!=1，跳过写库种子');
    return;
  }
  await goto('/business/erp/payable');
  await openPayableByNo(VERIFY_PAYABLE);
  const payBtn = page.locator('.t-drawer:visible').getByRole('button', { name: /发起付款/ }).first();
  if (!(await payBtn.count())) throw new Error('无「发起付款」按钮');
  await payBtn.click();
  await page.locator('[data-testid="payable-pay-dialog"]').waitFor({ state: 'visible', timeout: 8000 });
  await page.waitForTimeout(700);
  const amt = page.locator('[data-testid="payable-pay-amount"] input');
  await amt.click();
  await amt.fill('30');
  await amt.press('Tab');
  const method = await pickSelect(page.locator('[data-testid="payable-pay-method"]'), '银行转账');
  note(`付款方式=${method}`);
  await shot('seed-payment-filled');
  await confirmDialog(3000);
  note(`登记付款返回：${(await toastText()) || '(无 toast)'}`);
  await page.waitForTimeout(2000);
  await shot('seed-payment-after');
};

// ---------- 种子：上传附件（一次性写库） ----------
PH.seed_attach = async () => {
  useDir('seed-attach');
  if (!WRITE) {
    note('MANUAL_WRITE!=1，跳过写库种子');
    return;
  }
  const asset = process.env.MANUAL_ASSET || '/opt/cloudpivot/docs/manual/assets/发票样例-202611.pdf';
  if (!fs.existsSync(asset)) throw new Error(`样例文件不存在：${asset}`);
  await goto('/business/storage/file');
  const input = page.locator('input[type="file"]').first();
  await input.setInputFiles(asset);
  await page.waitForTimeout(3000);
  note(`文件管理上传返回：${(await toastText()) || '(无 toast)'}`);
  await shot('seed-storage-upload');

  await openPayableByNo(VERIFY_PAYABLE);
  const panelInput = page.locator('.t-drawer:visible input[type="file"]').first();
  await panelInput.setInputFiles(asset);
  await page.waitForTimeout(3000);
  note(`单据附件上传返回：${(await toastText()) || '(无 toast)'}`);
  const panel = page.locator('.doc-attach').first();
  const text = ((await panel.innerText().catch(() => '')) || '').replace(/\s+/g, ' ').slice(0, 160);
  note(`附件面板=${text}`);
  await shot('seed-attach-uploaded');
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
