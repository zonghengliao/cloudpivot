/**
 * t_cea00ecf 真机验收脚本（Playwright，仓库外运行）。
 *
 * 环境：vite dev :5212（源码 = /root/cea00ecf-wt/frontend 工作树，改后的 BatchActionBar）
 *      → 隔离后端 :18126（debug 二进制，库 cloudpivot_cea00ecf = cloudpivot_deadmin 的克隆）。
 *
 * 用例：
 *  A 应付页：勾选 → 批量导出 → 结果弹窗（已提交导出任务 …），关闭后工具栏隐藏；
 *  B 收货页：2 条 DRAFT 勾选后，其中 1 条被并发作废 → 批量作废 → 成功 1 / 失败 1 + 失败清单；
 *  C 重新勾选：工具栏回来、结果弹窗仍是关的、上一次结果已清（标题回到默认）。
 */
import { chromium } from '@playwright/test';
import fs from 'node:fs';
import path from 'node:path';

const BASE = 'http://127.0.0.1:5212';
const API = 'http://127.0.0.1:18126';
const OUT = '/root/cea00ecf-wt/docs/evidence/20261105-batch-result-dialog';
fs.mkdirSync(OUT, { recursive: true });

const report = { env: { base: BASE, api: API, startedAt: new Date().toISOString() }, cases: {}, console: [], pageErrors: [] };

async function login() {
  const resp = await fetch(`${API}/api/system/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username: 'admin', password: 'Admin@123456' }),
  });
  const body = await resp.json();
  if (body.code !== 0) throw new Error(`登录失败: ${JSON.stringify(body)}`);
  return body.data;
}

/** 直接打接口（模拟另一个用户并发改同一条单）。 */
async function apiPost(token, url, payload) {
  const resp = await fetch(`${API}${url}`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${token}` },
    body: JSON.stringify(payload),
  });
  return { status: resp.status, body: await resp.json() };
}

/** 结果弹窗容器（data-testid 落在 .t-dialog__ctx 上）。 */
const resultCtx = (page, testid) => page.locator(`[data-testid="${testid}"]`);

async function dialogVisible(page, testid) {
  const inner = resultCtx(page, testid).locator('.t-dialog').first();
  return (await inner.count()) > 0 && (await inner.isVisible());
}

async function dialogText(page, testid) {
  const ctx = resultCtx(page, testid);
  const header = (await ctx.locator('.t-dialog__header').first().innerText().catch(() => '')) || '';
  const body = (await ctx.locator('.t-dialog__body').first().innerText().catch(() => '')) || '';
  return { header: header.trim(), body: body.trim() };
}

async function waitGone(page, selector, timeout = 8000) {
  await page.waitForFunction((sel) => !document.querySelector(sel), selector, { timeout });
}

async function barGone(page, testid) {
  return (await page.locator(`[data-testid="${testid}"]`).count()) === 0;
}

/** 点某一行（按文本定位）的勾选框。 */
async function checkRow(page, tableTestid, text) {
  const row = page.locator(`[data-testid="${tableTestid}"] tbody tr`).filter({ hasText: text }).first();
  await row.waitFor({ state: 'visible', timeout: 15000 });
  const box = row.locator('.t-checkbox').first();
  await box.click();
}

async function closeResult(page, testid) {
  const ctx = resultCtx(page, testid);
  await ctx.locator('.t-dialog__close').first().click();
}

const token = await login();
report.env.mustChangePassword = token.must_change_password;

const browser = await chromium.launch();
const context = await browser.newContext({ viewport: { width: 1600, height: 1000 } });
await context.addInitScript(
  ([access, refresh]) => {
    localStorage.setItem('x_access_token', access);
    localStorage.setItem('x_refresh_token', refresh);
    localStorage.setItem('cp-onboarding-done', '1');
    sessionStorage.setItem('login-unread-shown', '1');
  },
  [token.access_token, token.refresh_token]
);
const page = await context.newPage();
page.on('console', (msg) => {
  if (msg.type() === 'error') report.console.push(msg.text());
});
page.on('pageerror', (err) => report.pageErrors.push(String(err)));

try {
  // ======================= A：批量导出成功结果 =======================
  const exportResponses = [];
  page.on('response', async (resp) => {
    if (resp.url().includes('/batch-export') || resp.url().includes('/impexp/export')) {
      let body = null;
      try { body = await resp.json(); } catch { /* 非 JSON */ }
      exportResponses.push({ url: resp.url(), status: resp.status(), body });
    }
  });

  await page.goto(`${BASE}/business/erp/payable`, { waitUntil: 'domcontentloaded' });
  await page.waitForSelector('[data-testid="payable-table"] tbody tr', { timeout: 30000 });
  await page.waitForFunction(
    () => /共\s*[1-9]\d*\s*条/.test(document.querySelector('[data-testid="payable-meta"]')?.textContent ?? ''),
    null,
    { timeout: 20000 }
  );

  report.cases.export_before = {
    barPresent: !(await barGone(page, 'payable-batch')),
    resultVisible: await dialogVisible(page, 'payable-batch-result'),
  };

  const firstRowText = await page.locator('[data-testid="payable-table"] tbody tr').first().innerText();
  await page.locator('[data-testid="payable-table"] tbody tr').first().locator('.t-checkbox').first().click();
  await page.waitForSelector('[data-testid="payable-batch"]', { state: 'visible', timeout: 10000 });
  report.cases.export_before.firstRow = firstRowText.split('\n').slice(0, 2).join(' ');

  await page.locator('[data-testid="payable-batch-export"]').click();
  const shown = await page
    .waitForFunction(
      () => {
        const ctx = document.querySelector('[data-testid="payable-batch-result"]');
        const dlg = ctx?.querySelector('.t-dialog');
        return !!dlg && dlg.offsetParent !== null;
      },
      null,
      { timeout: 20000 }
    )
    .then(() => true)
    .catch(() => false);

  const exportDialog = await dialogText(page, 'payable-batch-result');
  report.cases.export = {
    dialogShown: shown,
    header: exportDialog.header,
    body: exportDialog.body,
    barStillMounted: !(await barGone(page, 'payable-batch')),
    barVisible: await page.locator('[data-testid="payable-batch"]').isVisible().catch(() => false),
    requests: exportResponses.map((r) => ({ url: r.url, status: r.status, task_no: r.body?.data?.task_no, total: r.body?.data?.total })),
    // 勾选已被 execute() 清空 → 工具栏计数文案应是「未勾选」
    countText: (await page.locator('[data-testid="payable-batch-count"]').innerText().catch(() => '')).trim(),
  };
  await page.waitForTimeout(700);
  await page.screenshot({ path: path.join(OUT, 'A-export-result-dialog.png') });

  await closeResult(page, 'payable-batch-result');
  await waitGone(page, '[data-testid="payable-batch"]');
  report.cases.export_after_close = {
    barGone: await barGone(page, 'payable-batch'),
    resultVisible: await dialogVisible(page, 'payable-batch-result'),
  };

  // C：重新勾选 → 工具栏回来，弹窗仍是关的、上一次结果已清
  await page.locator('[data-testid="payable-table"] tbody tr').first().locator('.t-checkbox').first().click();
  await page.waitForSelector('[data-testid="payable-batch"]', { state: 'visible', timeout: 10000 });
  await page.waitForTimeout(300);
  const stale = await dialogText(page, 'payable-batch-result');
  report.cases.reselect = {
    barVisible: await page.locator('[data-testid="payable-batch"]').isVisible(),
    resultVisible: await dialogVisible(page, 'payable-batch-result'),
    headerAfterReselect: stale.header,
    headerHoldsOldExport: stale.header.includes('已提交导出任务'),
    bodyAfterReselect: stale.body,
  };
  // 清掉勾选，避免影响后面
  await page.locator('[data-testid="payable-batch-clear"]').click();
  await waitGone(page, '[data-testid="payable-batch"]');

  // ======================= B：批量改状态 部分失败 =======================
  await page.goto(`${BASE}/business/erp/stockin`, { waitUntil: 'domcontentloaded' });
  await page.waitForSelector('[data-testid="stockin-table"] tbody tr', { timeout: 30000 });
  await page.waitForFunction(
    () => (document.querySelector('[data-testid="stockin-table"]')?.innerText ?? '').includes('IN-20261031-000001'),
    null,
    { timeout: 20000 }
  );

  const draftNos = await page.evaluate(async () => {
    const res = await fetch('/api/business/erp/stock-in/page?page=1&page_size=50', {
      headers: { Authorization: `Bearer ${localStorage.getItem('x_access_token')}` },
    });
    const body = await res.json();
    return (body.data?.list ?? [])
      .filter((r) => r.status === 'DRAFT')
      .slice(0, 2)
      .map((r) => r.in_no);
  });
  if (draftNos.length < 2) throw new Error(`需要 2 条 DRAFT 收货单做部分失败用例，实际 ${draftNos.length} 条`);
  for (const no of draftNos) await checkRow(page, 'stockin-table', no);
  await page.waitForSelector('[data-testid="stockin-batch"]', { state: 'visible', timeout: 10000 });
  report.cases.partial_before = {
    draftNos,
    countText: (await page.locator('[data-testid="stockin-batch-count"]').innerText()).trim(),
  };

  // 并发作废第一条（模拟别人先动过：列表还是 DRAFT，后端已 CANCELLED）
  const idsOfDrafts = await page.evaluate(async (nos) => {
    const res = await fetch('/api/business/erp/stock-in/page?page=1&page_size=50', {
      headers: { Authorization: `Bearer ${localStorage.getItem('x_access_token')}` },
    });
    const body = await res.json();
    return (body.data?.list ?? []).filter((r) => nos.includes(r.in_no)).map((r) => ({ id: r.id, no: r.in_no, status: r.status }));
  }, draftNos);
  report.cases.partial_before.rows = idsOfDrafts;
  const raced = idsOfDrafts.find((r) => r.no === draftNos[0]);
  const race = await apiPost(token.access_token, '/api/business/erp/stock-in/batch-status', {
    ids: [raced.id],
    status: 'CANCELLED',
    reason: '并发验收：另一个用户先作废',
  });
  report.cases.partial_before.race = { no: raced.no, status: race.status, result: race.body?.data };

  await page.locator('[data-testid="stockin-batch-cancel"]').click();
  await page.waitForSelector('[data-testid="stockin-batch-confirm"] .t-dialog', { state: 'visible', timeout: 10000 });
  // t-textarea 受控：走原生 setter + input/change
  await page.evaluate(() => {
    const el = document.querySelector('[data-testid="stockin-batch-reason"] textarea')
      || document.querySelector('[data-testid="stockin-batch-reason"]');
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set;
    setter.call(el, '验收：并发作废后批量作废');
    el.dispatchEvent(new Event('input', { bubbles: true }));
    el.dispatchEvent(new Event('change', { bubbles: true }));
    el.dispatchEvent(new Event('blur', { bubbles: true }));
  });
  await page.locator('[data-testid="stockin-batch-confirm"] .t-dialog__footer button.t-button--theme-primary').first().click();

  const partialShown = await page
    .waitForFunction(
      () => {
        const ctx = document.querySelector('[data-testid="stockin-batch-result"]');
        const dlg = ctx?.querySelector('.t-dialog');
        return !!dlg && dlg.offsetParent !== null;
      },
      null,
      { timeout: 20000 }
    )
    .then(() => true)
    .catch(() => false);
  const partialDialog = await dialogText(page, 'stockin-batch-result');
  const failureList = await page
    .locator('[data-testid="stockin-batch-failure-list"] li')
    .allInnerTexts()
    .catch(() => []);
  report.cases.partial = {
    dialogShown: partialShown,
    header: partialDialog.header,
    body: partialDialog.body,
    failures: failureList,
    barStillMounted: !(await barGone(page, 'stockin-batch')),
  };
  await page.waitForTimeout(700);
  await page.screenshot({ path: path.join(OUT, 'B-partial-failure-dialog.png') });

  await closeResult(page, 'stockin-batch-result');
  await waitGone(page, '[data-testid="stockin-batch"]');
  report.cases.partial_after_close = {
    barGone: await barGone(page, 'stockin-batch'),
    resultVisible: await dialogVisible(page, 'stockin-batch-result'),
  };
} catch (err) {
  report.error = String(err?.stack ?? err);
  try { await page.screenshot({ path: path.join(OUT, 'ZZ-error.png') }); } catch { /* ignore */ }
} finally {
  report.console = report.console.filter((t) => !t.includes('notice/stream'));
  report.finishedAt = new Date().toISOString();
  fs.writeFileSync(path.join(OUT, 'report.json'), JSON.stringify(report, null, 2));
  console.log(JSON.stringify(report, null, 2));
  await browser.close();
}
