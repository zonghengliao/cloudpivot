/**
 * P2-8-F1 真机验收（补）：项目被软删后的显示回退 + 编辑态兜底选项。
 *
 * 场景：建单时挂项目 → 项目被软删 → 列表/详情显示「（已删除项目）」（不是 uuid、不是空白），
 * 编辑弹窗里下拉仍钉住当前值（显示「（已删除项目）（当前所属项目）」），不会静默变空。
 * 顺带记录：这种单去保存会被后端 400+66002 顶回（后端契约：非空必须是存在且未软删的项目）。
 */
import { chromium } from '@playwright/test';
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';

const BASE = process.env.BASE ?? 'http://127.0.0.1:5183';
const API = 'http://127.0.0.1:18157';
const TOKEN = readFileSync('/root/.hermes/profiles/frontend-developer/cache/scratch/token.txt', 'utf8').trim();
const REFRESH = readFileSync('/root/.hermes/profiles/frontend-developer/cache/scratch/refresh.txt', 'utf8').trim();
const OUT = process.env.OUT ?? '/root/.hermes/profiles/frontend-developer/cache/scratch/p28fe-out';
mkdirSync(OUT, { recursive: true });

const log = [];
const say = (line) => {
  log.push(line);
  console.log(line);
};

async function api(method, path, body) {
  const resp = await fetch(`${API}/api${path}`, {
    method,
    headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${TOKEN}` },
    body: body ? JSON.stringify(body) : undefined,
  });
  return { status: resp.status, body: await resp.json() };
}

// ---------- 造数：项目 + 挂在项目上的采购单，然后软删项目 ----------
const proj = await api('POST', '/business/project', { name: 'P28FE 待删项目', code: 'P28FE-PO-DEL' });
say(`① 建项目：${proj.status} ${JSON.stringify(proj.body.data)}`);
const projectId = proj.body.data.id;

const order = await api('POST', '/business/erp/purchase-order', {
  purchase_type: 'SPOT',
  supplier_id: 'b2b10001-0000-4000-8000-000000000014',
  warehouse_id: 'b1b00002-0000-4000-8000-000000000001',
  order_date: '2026-11-05',
  price_mode: 'EXCL',
  credit_days: 0,
  project_id: projectId,
  items: [{ material_id: 'd1370003-0000-4000-8000-000000000001', qty: 1, price_excl: 50, tax_rate: 13 }],
});
say(`② 建单（带项目）：${order.status} ${order.body.code} ${order.body.data?.order?.order_no}`);
const orderId = order.body.data.order.id;
const orderNo = order.body.data.order.order_no;

const del = await api('DELETE', `/business/project/${projectId}`);
say(`③ 软删项目：${del.status} ${JSON.stringify(del.body.data ?? del.body.msg)}`);

const after = await api('GET', `/business/erp/purchase-order/page?page=1&page_size=200&project_id=${projectId}`);
const row = (after.body.data?.list ?? []).find((r) => r.order_no === orderNo) ?? null;
say(`④ 软删后接口回读：${JSON.stringify({ project_id: row?.project_id ?? null, project_name: row?.project_name ?? null })}`);

const browser = await chromium.launch();
const context = await browser.newContext({ viewport: { width: 1600, height: 1000 }, locale: 'zh-CN' });
await context.addInitScript(
  ([token, refresh]) => {
    localStorage.setItem('x_access_token', token);
    localStorage.setItem('x_refresh_token', refresh);
    localStorage.setItem('cp-onboarding-done', '1');
    sessionStorage.setItem('login-unread-shown', '1');
  },
  [TOKEN, REFRESH],
);
const page = await context.newPage();
page.on('pageerror', (err) => say(`[pageerror] ${err.message}`));

try {
  await page.goto(`${BASE}/business/erp/purchase`, { waitUntil: 'domcontentloaded' });
  await page.locator('[data-testid="purchase-table"]').waitFor({ state: 'visible', timeout: 30000 });

  const cell = page.locator(`[data-testid="purchase-project-${orderNo}"]`);
  await cell.waitFor({ state: 'visible', timeout: 20000 });
  const cellText = (await cell.innerText()).trim();
  say(`⑤ 列表「项目」列 = ${JSON.stringify(cellText)}`);
  if (cellText !== '（已删除项目）') throw new Error(`应为「（已删除项目）」，实际「${cellText}」`);
  await page.screenshot({ path: `${OUT}/06-list-deleted-project.png` });

  await page.locator(`[data-testid="btn-view-${orderNo}"]`).click();
  await page.locator('[data-testid="purchase-detail-project"]').waitFor({ state: 'visible', timeout: 20000 });
  await page.waitForTimeout(800);
  const detailText = (await page.locator('[data-testid="purchase-detail-project"]').innerText()).trim();
  say(`⑥ 详情「所属项目」= ${JSON.stringify(detailText)}`);
  if (detailText !== '（已删除项目）') throw new Error(`详情应为「（已删除项目）」，实际「${detailText}」`);
  await page.screenshot({ path: `${OUT}/07-detail-deleted-project.png` });

  const drawer = page.locator('.t-drawer');
  if ((await drawer.getAttribute('class'))?.includes('t-drawer--open')) {
    await page.locator('.t-drawer__mask').click({ position: { x: 5, y: 5 } });
  }
  await page.waitForFunction(
    () => !(document.querySelector('.t-drawer')?.getAttribute('class') ?? '').includes('t-drawer--open'),
    undefined,
    { timeout: 10000 },
  );
  await page.waitForTimeout(400);

  await page.locator(`[data-testid="btn-edit-${orderNo}"]`).click();
  await page.locator('[data-testid="purchase-form-dialog"]').waitFor({ state: 'visible', timeout: 15000 });
  await page.waitForTimeout(1200);
  const shown = (await page.locator('[data-testid="purchase-project"] input').first().inputValue()).trim();
  say(`⑦ 编辑弹窗里项目选择器显示 = ${JSON.stringify(shown)}`);
  if (!shown.includes('当前所属项目')) throw new Error('兜底选项没生效（选择器静默变空 → 一保存就清空归属）');
  await page.screenshot({ path: `${OUT}/08-form-deleted-project-fallback.png` });

  // 顺带记录：这种单去保存会被后端顶回（后端契约要求 project_id 非空时必须存在且未软删）
  await page.locator('[data-testid="btn-purchase-save-draft"]').click();
  await page.waitForTimeout(2500);
  const err = await page.evaluate(() => {
    const alert = document.querySelector('.dlg-alert');
    return alert ? alert.textContent.trim() : '';
  });
  say(`⑧ 保存回执（后端拒收已删项目，契约预期）：${JSON.stringify(err)}`);

  say('ALL PASS ✅');
} catch (error) {
  say(`FAIL ❌ ${error?.message ?? error}`);
  await page.screenshot({ path: `${OUT}/99b-failure.png` }).catch(() => {});
  process.exitCode = 1;
} finally {
  writeFileSync(`${OUT}/ui-flow-deleted.txt`, log.join('\n') + '\n');
  await browser.close();
}
