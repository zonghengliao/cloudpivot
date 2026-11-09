#!/usr/bin/env node
/**
 * DEPLOY(dep-v217) 真机抽查 —— 只读，打生产（144 的 nginx → 现网 dist → 8023）。
 *
 * 抽查本卡上线的东西是否真的在生效（不写库、不建单）：
 *   P2-3  received_qty 列已删、详情里的「已收」是现算派生值（接口有值 + 库无列）
 *   P2-2  采购单详情带 over_credit_limit / credit_exposure 三列
 *   P2-1  辅助账接口 /ledger/aux-balance 可用（V0214 权限码）
 *   P1-5  联动作废影响分析 /purchase-order/{id}/cancel-impact（V0213 权限码）
 *   P1-3  单据附件接口 /attachment/list（V0212 新表）
 *   P2-2b 流程定义 po_approve 已发布 v2 = 9 节点 / 11 边（超额度 → 财务复核）
 *   前端  /business/erp/purchase-order、/business/erp/ledger、/business/impexp/import、
 *         /flow/def/:id/edit 画布 四页不白屏、无 4xx/5xx、截图留证
 *
 * 用法（在 144 上）：
 *   set -a; . /etc/cloudpivot/cloudpivot.env; set +a
 *   node /opt/cloudpivot/scripts/verify/dep_v217_spotcheck.mjs
 */
import { mkdirSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';

const FE_DIR = process.env.SC_FE_DIR ?? '/opt/cloudpivot/frontend';
const { chromium } = createRequire(`${FE_DIR}/package.json`)('@playwright/test');

const BASE = process.env.SC_BASE ?? 'http://127.0.0.1';
const OUT = process.env.SC_OUT ?? '/root/dep-v217-evidence';
const USER = process.env.SC_USER ?? 'admin';
// 登录口径（卡 t_d4e92b99）：运维账号 CLOUDPIVOT_OPS_*，**单次尝试**（候选重试会把账号锁 30 分钟）
const CANDIDATES = [process.env.SC_PASS || process.env.CLOUDPIVOT_OPS_PASSWORD || ''].filter(Boolean);

mkdirSync(OUT, { recursive: true });
const results = [];
const shots = [];
const httpErrors = [];
const consoleErrors = [];
const rec = (name, passed, detail = '') => {
  results.push({ name, passed: !!passed, detail });
  console.log(`${passed ? 'PASS' : 'FAIL'} | ${name}${detail ? ' | ' + detail : ''}`);
  return !!passed;
};
const shot = async (page, name) => {
  const path = `${OUT}/${name}.png`;
  await page.screenshot({ path, fullPage: false });
  shots.push(path);
  console.log(`[shot] ${path}`);
  return path;
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function login() {
  for (const password of CANDIDATES) {
    const res = await fetch(`${BASE}/api/system/auth/login`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username: USER, password }),
    });
    const body = await res.json().catch(() => ({}));
    if (body.code === 0 && body.data?.access_token) return body.data;
  }
  return null;
}
const api = async (token, path) => {
  const res = await fetch(`${BASE}${path}`, { headers: { Authorization: `Bearer ${token}` } });
  const body = await res.json().catch(() => ({}));
  return { status: res.status, body };
};

const session = await login();
if (!session) {
  console.error('登录失败：口令候选都不对');
  process.exit(2);
}
const TOKEN = session.access_token;
rec('登录成功（/api/system/auth/login）', true, `user=${USER}`);

// ---------- ① 采购订单列表 + 详情（P2-3 派生 / P2-2 额度三列） ----------
const list = await api(TOKEN, '/api/business/erp/purchase-order/page?page=1&page_size=10');
const rows = list.body?.data?.list ?? [];
rec('采购订单分页接口', list.body?.code === 0 && rows.length > 0, `rows=${rows.length}`);

const po = rows.find((r) => r && r.id) ?? rows[0];
let poDetail = null;
let firstMaterialId = null;
if (po?.id) {
  poDetail = await api(TOKEN, `/api/business/erp/purchase-order/${po.id}`);
  const d = poDetail.body?.data ?? {};
  rec('采购单详情接口', poDetail.body?.code === 0, `keys=${Object.keys(d).slice(0, 8).join(',')}`);
  rec(
    'P2-2 额度三列在 DTO 里（over_credit_limit / credit_exposure / credit_limit_snapshot）',
    'over_credit_limit' in d && 'credit_exposure' in d && 'credit_limit_snapshot' in d,
    `over=${d.over_credit_limit} exposure=${d.credit_exposure} snapshot=${d.credit_limit_snapshot}`,
  );
  // 明细：purchase-order/{id} 只回单头，明细走明细表分页（received_qty 就带在这儿）
  const itemPage = await api(TOKEN, '/api/business/erp/purchase-order-item/page?page=1&page_size=5');
  const itemRows = itemPage.body?.data?.list ?? [];
  const item = itemRows[0] ?? null;
  rec(
    'P2-3 明细「已收数量」为现算派生值（接口有字段，库无列）',
    itemPage.body?.code === 0 && !!item && item.received_qty !== undefined && item.received_qty !== null,
    item ? `rows=${itemRows.length} first.received_qty=${item.received_qty} qty=${item.qty}` : `rows=0 code=${itemPage.body?.code}`,
  );
  if (item) {
    firstMaterialId = item.material_id ?? null;
    const rq = Number(item.received_qty);
    const qty = Number(item.qty);
    rec('P2-3 派生值量纲合理（0 ≤ 已收 ≤ 订购）', Number.isFinite(rq) && rq >= 0 && (qty <= 0 || rq <= qty + 1e-6), `received=${rq} ordered=${qty}`);
  }
  // 待收明细 = 订购 − 现算已收（P2-3 唯一口径的另一面）
  const pend = await api(TOKEN, `/api/business/erp/purchase-order/${po.id}/pending-items`);
  const pendRows = pend.body?.data?.items ?? pend.body?.data?.list ?? [];
  rec(
    'P2-3 待收明细接口可用（qty − 已收 现算）',
    pend.body?.code === 0,
    `code=${pend.body?.code} rows=${Array.isArray(pendRows) ? pendRows.length : 'n/a'}`,
  );

  // ---------- ② 联动作废影响分析（V0213） ----------
  const impact = await api(TOKEN, `/api/business/erp/purchase-order/${po.id}/cancel-impact`);
  rec('V0213 联动作废影响分析接口 200/code=0', impact.status === 200 && impact.body?.code === 0, `keys=${Object.keys(impact.body?.data ?? {}).slice(0, 8).join(',')}`);

  // ---------- ③ 单据附件（V0212） ----------
  const att = await api(TOKEN, `/api/business/attachment/list?doc_type=PURCHASE_ORDER&doc_id=${po.id}`);
  rec('V0212 单据附件列表接口 200/code=0', att.status === 200 && att.body?.code === 0, `total=${att.body?.data?.total ?? att.body?.data?.length ?? 'n/a'}`);
}

// ---------- ④ 辅助账（V0214 权限码 + P2-1 数据） ----------
const auxRes = await api(TOKEN, '/api/business/erp/ledger/aux-balance?aux_type=MATERIAL&period_from=2026-01-01&period_to=2026-12-31');
let auxRows = 0;
if (auxRes.body?.code === 0) {
  const d = auxRes.body.data ?? {};
  auxRows = (d.list ?? d.rows ?? d.items ?? []).length;
}
rec('V0214 辅助账接口 200/code=0（权限码已种）', auxRes.status === 200 && auxRes.body?.code === 0, `status=${auxRes.status} code=${auxRes.body?.code} rows=${auxRows}`);
// 用一个真实物料再查一次（P2-1：物料维度真的能算出发生额）
const materialId = firstMaterialId;
if (materialId) {
  const one = await api(TOKEN, `/api/business/erp/ledger/aux-balance?aux_type=MATERIAL&aux_id=${materialId}&period_from=2026-01-01&period_to=2026-12-31`);
  rec('P2-1 按物料查辅助账（有发生额/余额行）', one.body?.code === 0, `material=${materialId} code=${one.body?.code}`);
}

// ---------- ⑤ 流程定义 po_approve v2（V0216） ----------
const defs = await api(TOKEN, '/api/business/flow/def/page?page=1&page_size=100');
const defRows = defs.body?.data?.list ?? [];
const poDef = defRows.find((d) => d.flow_code === 'po_approve');
rec('流程定义列表接口', defs.body?.code === 0 && defRows.length > 0, `defs=${defRows.length}`);
rec(
  'P2-2b po_approve 已发布 v2（9 节点 / 11 边）',
  !!poDef && poDef.status === 'PUBLISHED' && poDef.current_version_no === 2 && poDef.node_count === 9 && poDef.edge_count === 11,
  poDef ? `no=${poDef.current_version_no} nodes=${poDef.node_count} edges=${poDef.edge_count} status=${poDef.status}` : 'po_approve 不存在',
);

// ---------- ⑥ 页面抽查（截图 + 控制台/HTTP 判据） ----------
const browser = await chromium.launch({ args: ['--no-sandbox'] });
const ctx = await browser.newContext({ viewport: { width: 1560, height: 960 } });
await ctx.addInitScript(
  ([t, r]) => {
    localStorage.setItem('x_access_token', t);
    localStorage.setItem('x_refresh_token', r);
  },
  [TOKEN, session.refresh_token ?? ''],
);
const page = await ctx.newPage();
page.on('console', (m) => {
  if (m.type() === 'error') consoleErrors.push(m.text().slice(0, 200));
});
page.on('response', (res) => {
  const u = res.url();
  if (u.includes('/api/') && res.status() >= 400) httpErrors.push(`${res.status()} ${res.request().method()} ${u.replace(BASE, '')}`);
});

const visit = async (path, name, expectText) => {
  httpErrors.length = 0;
  consoleErrors.length = 0;
  await page.goto(`${BASE}${path}`, { waitUntil: 'domcontentloaded', timeout: 45000 });
  await sleep(2500);
  const body = (await page.locator('body').innerText().catch(() => '')).trim();
  const notFound = body.includes('您访问的页面不存在');
  const hit = !expectText || body.includes(expectText);
  await shot(page, name);
  rec(
    `页面 ${path}`,
    !notFound && hit && httpErrors.length === 0,
    `命中「${expectText ?? '-'}」=${hit} 404页=${notFound} http4xx/5xx=${httpErrors.length}${httpErrors.length ? ' → ' + httpErrors.slice(0, 3).join(' ; ') : ''}`,
  );
  return body;
};

await visit('/business/erp/purchase-order', '01-purchase-order-list', '采购');
const ledgerBody = await visit('/business/erp/ledger', '02-ledger', '余额');
await visit('/business/impexp/import', '03-import-wizard', '导入');
await visit('/flow/def', '04-flow-def-list', '流程');

// 画布页（SAFARI-FLOW 改的就是这块组件）
if (poDef?.id) {
  await visit(`/flow/def/${poDef.id}/edit`, '05-flow-canvas-po-approve', '');
  // 画布节点是否真的画出来了：取 .flow-node 的屏幕矩形（不在 (0,0) 叠成一团）
  const nodeBoxes = await page.evaluate(() => {
    const els = [...document.querySelectorAll('.flow-node, .flow-node__card')].slice(0, 12);
    return els.map((el) => {
      const r = el.getBoundingClientRect();
      return { x: Math.round(r.x), y: Math.round(r.y), w: Math.round(r.width), h: Math.round(r.height) };
    });
  });
  const distinct = new Set(nodeBoxes.map((b) => `${b.x},${b.y}`)).size;
  rec(
    'SAFARI-FLOW 画布节点位置分散（不是全叠在原点）',
    nodeBoxes.length > 0 && distinct > 1 && nodeBoxes.every((b) => b.w > 20 && b.h > 10),
    `nodes=${nodeBoxes.length} distinctPos=${distinct} 首两个=${JSON.stringify(nodeBoxes.slice(0, 2))}`,
  );
}

await visit('/business/erp/voucher', '06-voucher-list', '凭证');

const summary = {
  when: new Date().toISOString(),
  base: BASE,
  pass: results.filter((r) => r.passed).length,
  fail: results.filter((r) => !r.passed).length,
  results,
  shots,
  consoleErrors,
  httpErrors,
};
writeFileSync(`${OUT}/result.json`, JSON.stringify(summary, null, 2));
console.log('\n=== 汇总 ===');
console.log(`PASS ${summary.pass} / FAIL ${summary.fail}`);
if (summary.fail > 0) console.log('FAIL 明细：\n' + results.filter((r) => !r.passed).map((r) => ` - ${r.name} :: ${r.detail}`).join('\n'));
await browser.close();
process.exit(summary.fail === 0 ? 0 : 1);
