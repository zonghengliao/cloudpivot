#!/usr/bin/env node
/**
 * 【D3-UI】证据收集：接口返回样例 + 面板 DOM 文本矩阵 + PUT/DELETE 响应 + 撤回清场。
 * 只跑一次，产出文本证据（截图由 d3ui_state_edge_e2e.mjs 出）。
 */
import { writeFileSync } from 'node:fs';
import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';

const BASE = process.env.D3UI_BASE_URL || 'http://127.0.0.1';
const PW = process.env.NIK_ADMIN_PASSWORD || 'Admin@123456';
const OUT = process.env.D3UI_OUT_DIR || '/root/d3ui';
const ENTITY = 'STOCK_OUT';

const lr = await fetch(`${BASE}/api/system/auth/login`, {
  method: 'POST', headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ username: 'admin', password: PW }),
});
const lg = await lr.json();
const T = lg.data.access_token;
const H = { Authorization: `Bearer ${T}`, 'Content-Type': 'application/json' };
const api = async (method, path, body) => {
  const r = await fetch(`${BASE}${path}`, { method, headers: H, body: body ? JSON.stringify(body) : undefined });
  let j = null; try { j = await r.json(); } catch (e) { j = { parse_error: String(e) }; }
  return { status: r.status, body: j };
};

// ---------- ① GET 样例 ----------
const get1 = await api('GET', `/api/business/flow/state-edge?entity=${ENTITY}`);
writeFileSync(`${OUT}/d3ui-get-state-edge.json`, JSON.stringify(get1.body, null, 2));
console.log('== GET /api/business/flow/state-edge?entity=STOCK_OUT');
console.log(`   status=${get1.status} code=${get1.body.code} total=${get1.body.data.total}`);
const view = get1.body.data.list[0];
console.log(`   entity=${view.entity} label=${view.entity_label} base_edges=${view.base_edges.length} configs=${view.configs.length} effective=${view.effective_edges.length} pending=${view.pending_changes.length}`);
console.log('   边：');
for (const e of view.base_edges) {
  console.log(`     ${e.from} → ${e.to}  文案=${e.label}  mandatory=${e.mandatory} requires_reason=${e.requires_reason} auth_code=${e.auth_code} configured=${e.configured} enabled=${e.enabled}`);
}

// ---------- ② PUT 样例（真提交一张审批单，随后撤回） ----------
const put = await api('PUT', '/api/business/flow/state-edge', {
  entity: ENTITY,
  from_status: 'DRAFT',
  to_status: 'POSTED',
  enabled: false,
  requires_reason: true,
  label: null,
  auth_code: null,
  confirm: true,
  reason: 'D3-UI 证据：停用「直接过账」并追加必填原因（提交审批，不直接落库）',
});
writeFileSync(`${OUT}/d3ui-put-state-edge.json`, JSON.stringify(put.body, null, 2));
console.log('\n== PUT /api/business/flow/state-edge（真提交审批）');
console.log(`   status=${put.status} code=${put.body.code}`);
console.log(`   applied=${put.body.data?.applied} instance_no=${put.body.data?.instance_no} status=${put.body.data?.instance_status}`);
console.log(`   message=${put.body.data?.message}`);

// ---------- ③ 提交后：配置行没变、在跑审批 1 张 ----------
const get2 = await api('GET', `/api/business/flow/state-edge?entity=${ENTITY}`);
const v2 = get2.body.data.list[0];
console.log('\n== 提交后再 GET');
console.log(`   configs=${v2.configs.length}（未直接落库） pending_changes=${v2.pending_changes.length}`);
if (v2.pending_changes[0]) {
  const p = v2.pending_changes[0];
  console.log(`   ${p.instance_no} action=${p.action} ${p.from}→${p.to} summary=${p.summary}`);
}

// ---------- ④ 撤回清场 ----------
if (put.body.data?.instance_id) {
  const wd = await api('POST', `/api/business/flow/instance/${put.body.data.instance_id}/withdraw`, {
    reason: 'D3-UI 证据收集完毕，撤回验收用审批单',
  });
  console.log(`\n== 撤回审批单 status=${wd.status} code=${wd.body.code} msg=${wd.body.msg}`);
}
const get3 = await api('GET', `/api/business/flow/state-edge?entity=${ENTITY}`);
const v3 = get3.body.data.list[0];
console.log(`== 撤回后 configs=${v3.configs.length} pending_changes=${v3.pending_changes.length}（配置一分未动）`);

// ---------- ⑤ 面板 DOM 文本矩阵（深链 /flow/def?entity=STOCK_OUT） ----------
const browser = await chromium.launch();
const context = await browser.newContext({ viewport: { width: 1500, height: 1000 } });
await context.addInitScript(([a, r]) => {
  localStorage.setItem('x_access_token', a);
  localStorage.setItem('x_refresh_token', r);
  localStorage.setItem('x_guide_done', '1');
  localStorage.setItem('cp-onboarding-done', '1');
}, [lg.data.access_token, lg.data.refresh_token]);
const page = await context.newPage();
const errs = [];
page.on('console', (m) => { if (m.type() === 'error') errs.push(m.text()); });
page.on('pageerror', (e) => errs.push(String(e)));
await page.goto(`${BASE}/flow/def?entity=${ENTITY}`, { waitUntil: 'networkidle', timeout: 45000 });
await page.waitForTimeout(2500);

const drawer = page.locator('[data-testid="state-edge-drawer"]');
const drawerVisible = await drawer.isVisible().catch(() => false);
const box = drawerVisible ? await drawer.boundingBox() : null;
const table = page.locator('[data-testid="state-edge-table"]');
const tableBox = (await table.isVisible().catch(() => false)) ? await table.boundingBox() : null;
const cells = await page.locator('[data-testid="state-edge-table"] tbody tr').evaluateAll((trs) =>
  trs.map((tr) => Array.from(tr.querySelectorAll('td')).map((td) => td.innerText.replace(/\s+/g, ' ').trim())),
);
const drawerText = drawerVisible ? ((await drawer.innerText()) || '').replace(/\s+/g, ' ') : '';
const report = [
  `深链 /flow/def?entity=${ENTITY}`,
  `抽屉可见 = ${drawerVisible}  box = ${JSON.stringify(box)}`,
  `表格 box = ${JSON.stringify(tableBox)}`,
  `行数 = ${cells.length}`,
  '单元格矩阵：',
  ...cells.map((row, i) => `  ${i + 1}. ${row.join(' | ')}`),
  '',
  `抽屉文本 = ${drawerText}`,
  `控制台 error = ${JSON.stringify(errs.slice(0, 5))}`,
].join('\n');
writeFileSync(`${OUT}/d3ui-panel-dom.txt`, report);
console.log('\n== 深链 /flow/def?entity=STOCK_OUT 的 DOM 证据');
console.log(report);

await browser.close();
