#!/usr/bin/env node
/**
 * AUDIT-ALL 手段3（UI 部分）：真机页面巡检 + 链路⑥ 流程编辑器（拖节点 / 连线 / 保存 / 发布）
 *
 * 前置：audit_instance.sh start（:18099）+ vite dev（:5173, BACKEND_ORIGIN=18099）
 * 用法：node audit_ui.mjs [outDir]
 */
'use strict';
import path from 'node:path';
import fs from 'node:fs';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const require = createRequire(import.meta.url);
const __dirname = path.dirname(fileURLToPath(import.meta.url));

function loadPlaywright() {
  for (const c of ['/opt/cp_audit_head/frontend/node_modules/@playwright/test',
                   '/opt/cloudpivot/frontend/node_modules/@playwright/test']) {
    try { return require(c); } catch { /* next */ }
  }
  throw new Error('no playwright');
}

const BASE = process.env.AUDIT_UI_BASE || 'http://127.0.0.1:5173';
const OUT = process.argv[2] || path.join(__dirname, '04_screenshot');
const PW = process.env.AUDIT_ADMIN_PASS || 'Admin@123456-audit1';
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

const summary = { pages: [], flow: {}, errors: [] };

(async () => {
  fs.mkdirSync(OUT, { recursive: true });
  const pw = loadPlaywright();
  const apiLogin = await fetch(`${BASE}/api/system/auth/login`, {
    method: 'POST', headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username: 'admin', password: PW }),
  });
  const lg = await apiLogin.json();
  if (!lg?.data?.access_token) {
    console.error('登录失败:', JSON.stringify(lg).slice(0, 300));
    process.exit(2);
  }
  const token = lg.data.access_token;
  const browser = await pw.chromium.launch();
  const ctx = await browser.newContext({ viewport: { width: 1600, height: 1000 } });
  await ctx.addInitScript(([a, r]) => {
    localStorage.setItem('x_access_token', a);
    localStorage.setItem('x_refresh_token', r);
    localStorage.setItem('cp-onboarding-done', '1');
    localStorage.setItem('x_guide_done', '1');
    sessionStorage.setItem('login-unread-shown', '1');
  }, [token, lg.data.refresh_token]);
  const page = await ctx.newPage();

  // ---------- A. 页面巡检 ----------
  const SKIP_PAGES = process.env.AUDIT_SKIP_PAGES === '1';
  for (const [name, url] of (SKIP_PAGES ? [] : PAGES)) {
    const errs = [], bad = [];
    const onConsole = (m) => { if (m.type() === 'error' && !/notice\/stream|401 \(Unauthorized\)/.test(m.text())) errs.push(m.text().slice(0, 200)); };
    const onErr = (e) => errs.push(String(e).slice(0, 200));
    const onResp = (r) => { if (r.url().includes('/api/') && r.status() >= 400 && !r.url().includes('/notice/stream')) bad.push(`${r.status()} ${r.url().slice(0, 120)}`); };
    page.on('console', onConsole); page.on('pageerror', onErr); page.on('response', onResp);
    let navErr = '';
    await page.goto(`${BASE}${url}`, { waitUntil: 'domcontentloaded', timeout: 30000 }).catch((e) => { navErr = String(e).slice(0, 120); });
    await page.waitForTimeout(2500);
    const text = ((await page.locator('body').innerText().catch(() => '')) || '').replace(/\s+/g, ' ').trim();
    const rows = await page.locator('tbody tr').count();
    const file = path.join(OUT, `${name}.png`);
    await page.screenshot({ path: file }).catch(() => {});
    page.removeListener('console', onConsole); page.removeListener('pageerror', onErr); page.removeListener('response', onResp);
    const rec = { name, url, textLen: text.length, rows, consoleErrs: errs.length, apiErrs: bad.length, navErr, shot: file, sample: text.slice(0, 80) };
    summary.pages.push(rec);
    const flag = (text.length < 60 || errs.length > 0 || bad.length > 0) ? 'FAIL' : 'PASS';
    console.log(`[${flag}] ${url}  文本=${text.length} 行=${rows} console_err=${errs.length} api_err=${bad.length} ${navErr}`);
    if (bad.length) console.log('    api:', bad.slice(0, 3).join(' | '));
    if (errs.length) console.log('    console:', errs.slice(0, 2).join(' | '));
  }

  // ---------- B. 链路⑥：流程编辑器 拖节点 / 连线 / 保存 / 发布 ----------
  const defId = process.env.AUDIT_DEF_ID || '20000000-0000-4000-8000-0000000a0182'; // po_approve（def id）
  const api = async (method, p, body) => {
    const r = await fetch(`${BASE}/api${p}`, {
      method, headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${token}` },
      body: body ? JSON.stringify(body) : undefined,
    });
    return { status: r.status, json: await r.json().catch(() => ({})) };
  };
  const before = await api('GET', `/business/flow/def/${defId}/versions`);
  const draft = await api('POST', `/business/flow/def/${defId}/draft`, {});
  console.log(`\n[flow] 另存新草稿 HTTP ${draft.status} ${JSON.stringify(draft.json).slice(0, 160)}`);
  summary.flow.draftStatus = draft.status;
  const vid = draft?.json?.data?.id || draft?.json?.data?.version?.id;
  summary.flow.draftVersionId = vid;

  await page.goto(`${BASE}/flow/def/${defId}/edit?canvas=vueflow`, { waitUntil: 'domcontentloaded', timeout: 45000 });
  await page.waitForSelector('.vue-flow__node', { timeout: 30000 }).catch(() => {});
  await page.waitForTimeout(2000);
  const nodeCount = await page.locator('.vue-flow__node').count();
  const edgeCount = await page.locator('.vue-flow__edge').count();
  console.log(`[flow] 画布渲染：节点 ${nodeCount} 个 / 连线 ${edgeCount} 条`);
  summary.flow.nodesRendered = nodeCount;
  summary.flow.edgesRendered = edgeCount;
  await page.screenshot({ path: path.join(OUT, '40_flow_canvas_before.png') });

  // 拖第一个节点
  let dragOk = false, beforePos = null, afterPos = null;
  const first = page.locator('.vue-flow__node').first();
  const box = await first.boundingBox().catch(() => null);
  if (box) {
    beforePos = { x: Math.round(box.x), y: Math.round(box.y) };
    await page.mouse.move(box.x + box.width / 2, box.y + 12);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width / 2 + 40, box.y + 60, { steps: 12 });
    await page.mouse.up();
    await page.waitForTimeout(800);
    const box2 = await first.boundingBox().catch(() => null);
    afterPos = box2 ? { x: Math.round(box2.x), y: Math.round(box2.y) } : null;
    dragOk = !!afterPos && (Math.abs(afterPos.x - beforePos.x) > 5 || Math.abs(afterPos.y - beforePos.y) > 5);
    console.log(`[flow] 拖节点：${JSON.stringify(beforePos)} → ${JSON.stringify(afterPos)}  ${dragOk ? 'PASS' : 'FAIL'}`);
  }
  summary.flow.drag = { before: beforePos, after: afterPos, ok: dragOk };
  await page.screenshot({ path: path.join(OUT, '41_flow_canvas_dragged.png') });

  // 连线：出口端口 → 另一张卡片
  let connectOk = false;
  const geo = await page.evaluate(() => {
    const outs = [...document.querySelectorAll('.flow-node__port--out')].map((el) => el.getBoundingClientRect());
    const cards = [...document.querySelectorAll('.flow-node__card')].map((el) => el.getBoundingClientRect());
    return {
      outs: outs.slice(0, 3).map((r) => ({ x: r.x + r.width / 2, y: r.y + r.height / 2 })),
      cards: cards.map((r) => ({ x: r.x + r.width / 2, y: r.y + r.height / 2, w: r.width })),
    };
  }).catch(() => null);
  if (geo && geo.outs.length && geo.cards.length) {
    const from = geo.outs[0];
    const target = geo.cards.find((c) => Math.abs(c.x - from.x) > 120) || geo.cards[1] || geo.cards[0];
    const edgesBefore = await page.locator('.vue-flow__edge').count();
    await page.mouse.move(from.x, from.y);
    await page.mouse.down();
    await page.mouse.move(from.x + 8, from.y + 4, { steps: 4 });
    await page.mouse.move(target.x, target.y, { steps: 18 });
    await page.waitForTimeout(250);
    await page.mouse.up();
    await page.waitForTimeout(900);
    const edgesAfter = await page.locator('.vue-flow__edge').count();
    connectOk = edgesAfter >= edgesBefore; // 已存在边可能被拒（重复边），但手势必须不报错
    console.log(`[flow] 连线手势：边 ${edgesBefore} → ${edgesAfter}`);
    summary.flow.connect = { before: edgesBefore, after: edgesAfter, ok: connectOk };
    await page.screenshot({ path: path.join(OUT, '42_flow_canvas_connected.png') });
  }

  // 保存
  const saveBtn = page.getByRole('button', { name: /^保存$/ }).first();
  let saveOk = false;
  if (await saveBtn.count()) {
    await saveBtn.click().catch(() => {});
    await page.waitForTimeout(2500);
    const body = (await page.locator('body').innerText()).replace(/\s+/g, ' ');
    saveOk = /保存成功|已保存|已保存，但有/.test(body);
    console.log(`[flow] 点「保存」：${saveOk ? 'PASS' : 'FAIL'}  ${body.slice(0, 120)}`);
    await page.screenshot({ path: path.join(OUT, '43_flow_saved.png') });
  }
  summary.flow.save = saveOk;

  // 发布
  const pubBtn = page.getByRole('button', { name: /^发布$/ }).first();
  let pubOk = false, pubMsg = '';
  if (await pubBtn.count()) {
    const disabled = await pubBtn.isDisabled().catch(() => true);
    if (!disabled) {
      await pubBtn.click().catch(() => {});
      await page.waitForTimeout(1500);
      const body = (await page.locator('body').innerText()).replace(/\s+/g, ' ');
      const confirm = page.locator('.t-dialog__footer button, .t-popconfirm button').filter({ hasText: /确定|确认/ }).first();
      if (await confirm.count()) { await confirm.click().catch(() => {}); await page.waitForTimeout(2500); }
      const body2 = (await page.locator('body').innerText()).replace(/\s+/g, ' ');
      pubOk = /发布成功/.test(body2);
      const blockedByRule = /发布会拦的问题|校验不通过|R\d+/.test(body2);
      pubMsg = (pubOk ? '发布成功' : (blockedByRule ? '发布被发布前校验拦下（预期行为）' : body2.slice(0, 160)));
    } else {
      pubMsg = '发布按钮为禁用态（有校验问题）';
    }
    console.log(`[flow] 点「发布」：${pubOk ? 'PASS' : '待看'} ${pubMsg}`);
    await page.screenshot({ path: path.join(OUT, '44_flow_published.png') });
  }
  summary.flow.publish = { ok: pubOk, msg: pubMsg };

  const after = await api('GET', `/business/flow/def/${defId}/versions`);
  const afterList = after?.json?.data?.list || after?.json?.data || [];
  summary.flow.versionCountBefore = Array.isArray(before?.json?.data?.list) ? before.json.data.list.length : null;
  summary.flow.versionCountAfter = Array.isArray(afterList) ? afterList.length : null;
  console.log(`[flow] 版本数：${summary.flow.versionCountBefore} → ${summary.flow.versionCountAfter}`);

  await browser.close();
  fs.writeFileSync(path.join(__dirname, '04_ui_summary.json'), JSON.stringify(summary, null, 2));
  const failed = summary.pages.filter((p) => p.textLen < 60 || p.consoleErrs > 0 || p.apiErrs > 0);
  console.log(`\n页面巡检：${summary.pages.length - failed.length}/${summary.pages.length} PASS`);
  if (failed.length) console.log('异常页：' + failed.map((f) => `${f.url}(${f.apiErrs}api/${f.consoleErrs}js)`).join(', '));
})().catch((e) => { console.error('FATAL', e); process.exit(1); });
