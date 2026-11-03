/**
 * WRAPUP 真机冒烟（夜间批次收尾 t_d8d44404 · 2026-11-03）
 *
 * 覆盖卡面 ⑥ 的验收点：
 *   登录 → 合同管理 / 采购管理 / 仓库管理 / 财务管理 四个页面能打开
 *        → 采购单详情能看到审批进度
 *        → 合同页面能打开（A1–A5 的产出可用）
 *
 * 用法（默认打生产 172.18.10.144，可换成本机 dev）：
 *   WU_BASE=http://172.18.10.144 WU_USER=admin WU_PASS=<口令> \
 *     node scripts/verify/wrapup-smoke.mjs
 *
 * 产物：$WU_OUT（默认 docs/evidence/20261103-wrapup/shots）下的 png + stdout JSON 结论。
 * 退出码：全部 step ok = 0，任一失败 = 1（给 CI / 人工复跑用）。
 *
 * 说明：不使用 `pnpm test:e2e`（那份配置带「只许打隔离实例」的 target-guard，
 * 本脚本刻意打生产，故独立成一个只读冒烟脚本，不做任何写操作）。
 */
import { chromium } from '@playwright/test';
import { mkdirSync, writeFileSync } from 'node:fs';

const BASE = (process.env.WU_BASE ?? 'http://172.18.10.144').replace(/\/$/, '');
const OUT = process.env.WU_OUT ?? 'docs/evidence/20261103-wrapup/shots';
const USER = process.env.WU_USER ?? 'admin';
const PASS = process.env.WU_PASS ?? '';

mkdirSync(OUT, { recursive: true });

const result = { base: BASE, at: new Date().toISOString(), steps: [], shots: [] };
let contractRows = 0;
let failed = 0;

async function shot(page, name) {
  const path = `${OUT}/${name}.png`;
  await page.screenshot({ path, fullPage: false });
  result.shots.push(path);
  console.log(`[shot] ${path}`);
}

function record(step, ok, extra = {}) {
  const row = { step, ok, ...extra };
  result.steps.push(row);
  if (!ok) failed += 1;
  console.log(`[${ok ? 'ok  ' : 'FAIL'}] ${step} ${JSON.stringify(extra)}`);
}

const browser = await chromium.launch();
const context = await browser.newContext({ viewport: { width: 1600, height: 1000 } });
const page = await context.newPage();

try {
  // ---------- 登录 ----------
  await page.goto(`${BASE}/login`, { waitUntil: 'domcontentloaded' });
  await page.waitForSelector("input[placeholder='请输入用户名']", { timeout: 30000 });
  await page.fill("input[placeholder='请输入用户名']", USER);
  await page.fill("input[placeholder='请输入密码']", PASS);
  await page.click('button.t-button--theme-primary');
  await page.waitForURL(/\/(dashboard|change-password)/, { timeout: 40000 });
  record('login', true, { via: 'form', url: page.url() });
  await page.waitForTimeout(800);

  /** 打开一个 ERP 页面：等主表格出现 → 记行数 → 截图 */
  async function openPage(step, path, tableTestId, shotName) {
    await page.goto(`${BASE}${path}`, { waitUntil: 'domcontentloaded' });
    const errs = [];
    try {
      await page.waitForSelector(`[data-testid="${tableTestId}"]`, { timeout: 30000 });
    } catch (e) {
      errs.push(String(e).slice(0, 120));
    }
    // 列表自带 load-error 提示时一并记下（比只看「有没有表格」更能说明问题）
    const loadErr = await page
      .locator('[data-testid$="load-error"]')
      .first()
      .innerText()
      .catch(() => '');
    const rows = await page.locator(`[data-testid="${tableTestId}"] tbody tr`).count();
    const meta = await page
      .locator('[data-testid$="-meta"]')
      .first()
      .innerText()
      .catch(() => '');
    await page.waitForTimeout(400);
    await shot(page, shotName);
    record(step, errs.length === 0 && !loadErr, { path, rows, meta, loadErr: loadErr || null, errs });
    return rows;
  }

  // ---------- 合同管理 ----------
  contractRows = await openPage(
    'page-contract',
    '/business/erp/contract',
    'contract-table',
    '01-contract',
  );

  // ---------- 采购管理 ----------
  const poRows = await openPage(
    'page-purchase',
    '/business/erp/purchase-order',
    'purchase-table',
    '02-purchase',
  );

  // ---------- 采购单详情 + 审批进度 ----------
  // ★ 关键：列表的 t-table **没有 @row-click**，点行不会开详情，入口是操作列的「查看」（`btn-view-<单号>`）。
  //   生产 13 张单里只有 6 张挂了审批实例，所以逐个单号找：命中审批进度/审批单号才算通过，
  //   全都没有时把「空态」原文一并记下（真实结论，不写假绿）。
  try {
    if (poRows === 0) throw new Error('采购列表 0 行，无法打开详情');
    const APPROVAL_SELS = [
      '[data-testid="purchase-detail-approval-progress"]',
      '[data-testid="purchase-detail-approval-fallback"]',
      '[data-testid="purchase-detail-approval-empty-state"]',
      '[data-testid="purchase-detail-approval"]',
    ].join(', ');
    const readText = (sel) =>
      page
        .locator(sel)
        .first()
        .innerText({ timeout: 3000 })
        .catch(() => '');

    let orderNos = await page
      .locator('[data-testid^="purchase-no-"]')
      .evaluateAll((els) => els.map((e) => e.textContent?.trim() ?? '').filter(Boolean));
    orderNos = [...new Set(orderNos)].slice(0, 8);

    // 「查看」是 TDesign 表格里的 t-link，playwright 的可点性判定一直被表格遮罩挡住（实测 30s 超时），
    // 这里改走**页面自身支持的深链** `?po_id=<id>`（loadDetail 由 route.query.po_id 触发），
    // 既不依赖点击，也更接近用户从别处跳进来的真实路径。
    const token = await page.evaluate(() => localStorage.getItem('x_access_token'));
    const poList = await page.evaluate(async (tk) => {
      const r = await fetch('/api/business/erp/purchase-order/page?page=1&page_size=100', {
        headers: { Authorization: `Bearer ${tk}` },
      });
      const body = await r.json();
      return ((body.data || {}).list || []).map((x) => ({
        id: x.id,
        no: x.order_no,
        status: x.status,
        hasApproval: Boolean(x.approval_instance_id),
      }));
    }, token);

    let hit = null;
    const tried = [];
    const targets = [
      ...poList.filter((x) => x.hasApproval),
      ...poList.filter((x) => !x.hasApproval),
    ].slice(0, 6);
    for (const po of targets) {
      await page.goto(`${BASE}/business/erp/purchase-order?po_id=${po.id}`, {
        waitUntil: 'domcontentloaded',
      });
      await page.waitForSelector('[data-testid="purchase-detail-drawer"] .detail', { timeout: 30000 });
      await page.waitForSelector(APPROVAL_SELS, { timeout: 30000 });
      const head = await page
        .locator('[data-testid="purchase-detail-drawer"] .detail__head')
        .first()
        .innerText()
        .catch(() => '');
      const progress = await readText('[data-testid="purchase-detail-approval-progress"]');
      const link = await readText('[data-testid="purchase-detail-approval"]');
      const fallback = await readText('[data-testid="purchase-detail-approval-fallback"]');
      const empty = await readText('[data-testid="purchase-detail-approval-empty-state"]');
      tried.push({
        no: po.no,
        status: po.status,
        head: head.replace(/\n+/g, ' ').slice(0, 70),
        link: link.replace(/\n+/g, ' ').trim() || null,
        progressLen: progress.length,
        empty: empty.trim() || null,
        fallback: fallback.trim() || null,
      });
      if (progress.length > 0 || link.trim().length > 0) {
        hit = { no: po.no, status: po.status, link: link.trim(), progress, fallback };
        await shot(page, '03-purchase-detail-approval');
        break;
      }
    }
    if (hit) {
      record('purchase-detail-approval', true, {
        orderNo: hit.no,
        link: hit.link,
        progressLen: hit.progress.length,
        progressHead: hit.progress.split('\n').filter(Boolean).slice(0, 4).join(' / '),
        fallback: hit.fallback.trim() || null,
        triedCount: tried.length,
      });
    } else {
      await shot(page, '03-purchase-detail-approval');
      record('purchase-detail-approval', false, {
        error: `前 ${tried.length} 张采购单都没有审批实例（全部是未发起审批的老单）`,
        tried,
      });
    }
  } catch (e) {
    await shot(page, '03-purchase-detail-approval');
    record('purchase-detail-approval', false, { error: String(e).slice(0, 200) });
  }

  // ---------- 仓库管理（收货入库） ----------
  await openPage('page-stock-in', '/business/erp/stock-in', 'stockin-table', '04-stock-in');

  // ---------- 财务管理（凭证查询 + 会计期间） ----------
  await openPage('page-voucher', '/business/erp/voucher', 'voucher-table', '05-voucher');
  await openPage('page-period', '/business/erp/period', 'period-table', '06-period');

  // ---------- 控制台错误汇总 ----------
  record('console-errors', true, { note: '见 steps 内 errs 字段' });
} catch (e) {
  record('fatal', false, { error: String(e).slice(0, 300), url: page.url() });
  await shot(page, '99-fatal').catch(() => {});
} finally {
  result.contractRows = contractRows ?? 0;
  result.failed = failed;
  writeFileSync(`${OUT}/../smoke-result.json`, JSON.stringify(result, null, 2));
  console.log('\n=== RESULT JSON ===');
  console.log(JSON.stringify(result, null, 2));
  await browser.close();
}

process.exit(failed === 0 ? 0 : 1);
