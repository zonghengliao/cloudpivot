#!/usr/bin/env node
/**
 * P1-FIX-6 真机巡检：**39 个页面逐个走一遍，控制台/网络里不许出现一条 401**。
 *
 * # 与 `scripts/verify/audit_features_20261104/audit_ui.mjs` 的区别（那份不能当本卡证据）
 *
 * 那份审计把 `notice/stream` 的 401 **显式过滤掉了**：
 *   `! /notice\/stream|401 \(Unauthorized\)/.test(m.text())` —— 因为它当时是「已知缺陷」，
 *   过滤是为了让其它页面结论不被带偏。可本卡的验收项恰恰是「这条 401 必须消失」，
 *   所以这里的口径是**零豁免**：所有 `>=400` 的 `/api/` 响应全部落盘，逐条计数。
 *
 * # 每页采集
 *
 *   - `consoleErrors`：所有 `console.error`（含浏览器打印的 `Failed to load resource: 401`）；
 *   - `badApi`：所有 `status >= 400` 的 `/api` 响应（**不过滤 notice/stream**）；
 *   - `stream`：本页 `/api/business/notice/stream` 的响应状态（应为 200 且仅 200）；
 *   - 截图 + 正文长度 + 表格行数（证明确实渲染了，不是白屏混过）。
 *
 * # 用法
 *
 *   node page_walk.mjs <outDir> <roundLabel>
 *   环境：WALK_BASE（默认 http://127.0.0.1:5140） · PW_FILE（默认 /tmp/p1fix6-iso/admin.pw）
 */
'use strict';
import path from 'node:path';
import fs from 'node:fs';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const __dirname = path.dirname(fileURLToPath(import.meta.url));

function loadPlaywright() {
  for (const c of [
    '/opt/cloudpivot/frontend/node_modules/@playwright/test',
    '/opt/cp_audit_head/frontend/node_modules/@playwright/test',
  ]) {
    try {
      return require(c);
    } catch {
      /* next */
    }
  }
  throw new Error('找不到 @playwright/test');
}

const BASE = process.env.WALK_BASE || 'http://127.0.0.1:5140';
const PW_FILE = process.env.PW_FILE || '/tmp/p1fix6-iso/admin.pw';
const OUT = process.argv[2] || path.join(__dirname, '..', 'shots');
const ROUND = process.argv[3] || 'r1';
const PASSWORD = fs.readFileSync(PW_FILE, 'utf8').trim();

/** 39 页清单（与审计 `audit_ui.mjs` 的 PAGES 逐条一致，便于对账） */
const PAGES = [
  ['01_dashboard', '/dashboard'],
  ['02_task_list', '/task/list'],
  ['03_task_board', '/task/board'],
  ['04_task_gantt', '/task/gantt'],
  ['05_flow_def', '/flow/def'],
  ['06_flow_templates', '/flow/templates'],
  ['07_flow_task', '/flow/task'],
  ['08_erp_contract', '/business/erp/contract'],
  ['09_erp_po', '/business/erp/purchase-order'],
  ['10_erp_stockin', '/business/erp/stock-in'],
  ['11_erp_stockout', '/business/erp/stock-out'],
  ['12_erp_transfer', '/business/erp/transfer'],
  ['13_erp_inventory', '/business/erp/inventory'],
  ['14_erp_inventory_txn', '/business/erp/inventory-txn'],
  ['15_erp_payable', '/business/erp/payable'],
  ['16_erp_voucher', '/business/erp/voucher'],
  ['17_erp_ledger', '/business/erp/ledger'],
  ['18_erp_balance_sheet', '/business/erp/report/balance-sheet'],
  ['19_erp_income', '/business/erp/report/income-statement'],
  ['20_erp_cashflow', '/business/erp/report/cash-flow'],
  ['21_erp_period', '/business/erp/period'],
  ['22_erp_opening', '/business/erp/opening'],
  ['23_erp_year_end', '/business/erp/year-end'],
  ['24_erp_material', '/business/erp/material'],
  ['25_erp_supplier', '/business/erp/supplier'],
  ['26_erp_warehouse', '/business/erp/warehouse'],
  ['27_erp_account', '/business/erp/account'],
  ['28_system_user', '/system/user'],
  ['29_system_role', '/system/role'],
  ['30_system_menu', '/system/menu'],
  ['31_system_data_scope', '/system/data-scope'],
  ['32_system_field_mask', '/system/field-mask'],
  ['33_announcement', '/business/announcement'],
  ['34_notice', '/business/notice/message'],
  ['35_storage', '/business/storage/file'],
  ['36_impexp', '/business/impexp/import'],
  ['37_help', '/help'],
  ['38_changelog', '/system/change-log'],
  ['39_job', '/support/job'],
];

if (PAGES.length !== 39) {
  throw new Error(`页面清单应为 39 条，实际 ${PAGES.length}`);
}

(async () => {
  fs.mkdirSync(OUT, { recursive: true });
  const pw = loadPlaywright();

  // ---- 登录（真账号，走与浏览器同一条反代） ----
  const login = await fetch(`${BASE}/api/system/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username: 'admin', password: PASSWORD }),
  });
  const lg = await login.json();
  if (!lg?.data?.access_token) {
    console.error('登录失败：', JSON.stringify(lg).slice(0, 300));
    process.exit(2);
  }
  const token = lg.data.access_token;

  const browser = await pw.chromium.launch();
  const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  await ctx.addInitScript(([a, r]) => {
    localStorage.setItem('x_access_token', a);
    localStorage.setItem('x_refresh_token', r);
    localStorage.setItem('cp-onboarding-done', '1');
    localStorage.setItem('x_guide_done', '1');
    sessionStorage.setItem('login-unread-shown', '1');
  }, [token, lg.data.refresh_token]);

  const page = await ctx.newPage();
  const summary = { round: ROUND, base: BASE, pages: [], totals: {} };

  for (const [name, url] of PAGES) {
    const consoleErrors = [];
    const badApi = [];
    const stream = [];

    const onConsole = (m) => {
      // 零豁免：浏览器打印的 "Failed to load resource: ... 401" 也算
      if (m.type() === 'error') consoleErrors.push(m.text().slice(0, 240));
    };
    const onResp = (r) => {
      const u = r.url();
      if (!u.includes('/api/')) return;
      const status = r.status();
      if (u.includes('/api/business/notice/stream')) {
        stream.push(status);
      }
      if (status >= 400) {
        badApi.push({ status, url: u.replace(/\?.*$/, ''), query: /\?/.test(u) ? '?…' : '' });
      }
    };
    page.on('console', onConsole);
    page.on('response', onResp);

    let navErr = '';
    const streamResp = page
      .waitForResponse((r) => r.url().includes('/api/business/notice/stream'), { timeout: 15_000 })
      .catch(() => null);

    await page
      .goto(`${BASE}${url}`, { waitUntil: 'domcontentloaded', timeout: 40_000 })
      .catch((e) => {
        navErr = String(e).slice(0, 160);
      });
    const resp = await streamResp;
    await page.waitForTimeout(2_000); // 让这一页自己的接口 + 控制台噪声都落齐

    const bodyText = ((await page.locator('body').innerText().catch(() => '')) || '')
      .replace(/\s+/g, ' ')
      .trim();
    const rows = await page.locator('tbody tr').count().catch(() => 0);
    const shot = path.join(OUT, `${name}.jpg`);
    await page.screenshot({ path: shot, type: 'jpeg', quality: 55, fullPage: false }).catch(() => {});

    page.removeListener('console', onConsole);
    page.removeListener('response', onResp);

    const rec = {
      name,
      url,
      textLen: bodyText.length,
      rows,
      // ① 本页实时通道握手：200 = 连上；null = 15s 内没观察到（记下来，不假装）
      streamStatus: resp ? resp.status() : null,
      streamStatuses: stream,
      stream401: stream.filter((s) => s === 401).length,
      badApi,
      console401: consoleErrors.filter((t) => /401|Unauthorized/i.test(t)).length,
      consoleErrors,
      navErr,
      shot,
    };
    summary.pages.push(rec);

    const flag = rec.stream401 === 0 && badApi.length === 0 && rec.console401 === 0 && !navErr ? 'PASS' : 'FAIL';
    console.log(
      `[${flag}] ${url}  文本=${rec.textLen} 行=${rows} stream=${rec.streamStatus} ` +
        `stream401=${rec.stream401} api4xx=${badApi.length} console401=${rec.console401} ${navErr}`,
    );
    if (badApi.length) console.log('    api:', JSON.stringify(badApi.slice(0, 3)));
    if (rec.console401) console.log('    console:', consoleErrors.slice(0, 2).join(' | '));
  }

  summary.totals = {
    pages: summary.pages.length,
    stream401: summary.pages.reduce((n, p) => n + p.stream401, 0),
    api4xxPages: summary.pages.filter((p) => p.badApi.length > 0).length,
    console401: summary.pages.reduce((n, p) => n + p.console401, 0),
    stream200: summary.pages.filter((p) => p.streamStatus === 200).length,
    streamUnobserved: summary.pages.filter((p) => p.streamStatus === null).length,
    navErr: summary.pages.filter((p) => p.navErr).length,
  };
  const file = path.join(OUT, `walk_${ROUND}.json`);
  fs.writeFileSync(file, JSON.stringify(summary, null, 2));
  console.log(`\n=== ${ROUND} 汇总 ===`);
  console.log(JSON.stringify(summary.totals, null, 2));
  console.log(`明细：${file}`);

  await browser.close();
  const ok =
    summary.totals.stream401 === 0 &&
    summary.totals.console401 === 0 &&
    summary.totals.navErr === 0 &&
    summary.totals.stream200 === 39;
  console.log(ok ? `=== ${ROUND} 结论：PASS（39/39 页零 401，实时通道 200） ===` : `=== ${ROUND} 结论：FAIL ===`);
  process.exit(ok ? 0 : 1);
})().catch((e) => {
  console.error('巡检脚本异常：', e);
  process.exit(3);
});
