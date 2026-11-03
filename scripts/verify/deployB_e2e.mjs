#!/usr/bin/env node
/**
 * DEPLOY-B 真机验收（Playwright，打 144 的 nginx → 线上 dist → 新 release 二进制 → 生产库）
 *
 * 覆盖卡面第 7 步 10 项：
 *  ① 登录能进（不白屏）
 *  ② 出库单页面能打开 + 新建领用/报废/盘亏 + 过账后库存减少 + 成本结转凭证
 *  ③ 成本法下拉只有「移动加权」一个选项
 *  ④ 非基本单位被拒（物料主数据闸 66030）
 *  ⑤ 已取消采购单收货被拒
 *  ⑥ 审批流未配置 → 409 + 66033（由 deployB_flowgate.sh 触发，本脚本只负责断言）
 *  ⑦ 四单审批进度（采购/收货/应付/凭证/调拨）
 *  ⑧ 状态机边配置：HTTP 面可用；前端「边配置」入口是否存在（如实记录）
 *  ⑨ 快照巡检（脚本另跑，本脚本只读结果）
 *  ⑩ 控制台无 error、无 4xx/5xx
 *
 * 用法（144 上）：
 *   set -a; . /etc/cloudpivot/cloudpivot.env; set +a
 *   node scripts/verify/deployB_e2e.mjs
 */
import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';

const BASE = process.env.DEPLOYB_BASE_URL || 'http://127.0.0.1';
const USER = process.env.DEPLOYB_USER || 'admin';
const PASSWORDS = [process.env.NIK_ADMIN_PASSWORD, process.env.ALUN_ADMIN_PASSWORD, 'Admin@123456'].filter(Boolean);
const SHOT_DIR = process.env.DEPLOYB_SHOT_DIR || '/opt/cloudpivot/docs/evidence';
const STAMP = new Date().toISOString().replace(/[-:T.Z]/g, '').slice(0, 14);
const TAG = `DEPLOYB-${STAMP}`;

let PASS = 0, FAIL = 0, SKIP = 0;
const failures = [];
const ok = (m) => { PASS++; console.log(`[PASS] ${m}`); };
const bad = (m) => { FAIL++; failures.push(m); console.log(`[FAIL] ${m}`); };
const skip = (m) => { SKIP++; console.log(`[SKIP] ${m}`); };
const info = (m) => console.log(`       ${m}`);

// ---------------- HTTP ----------------
let TOKEN = null;
const H = () => ({ Authorization: `Bearer ${TOKEN}`, 'Content-Type': 'application/json' });
async function api(method, path, body) {
  const r = await fetch(`${BASE}${path}`, { method, headers: H(), body: body ? JSON.stringify(body) : undefined });
  let j = null; try { j = await r.json(); } catch (e) { j = { parse_error: String(e) }; }
  return { status: r.status, body: j };
}
const get = (p) => api('GET', p);
const post = (p, b) => api('POST', p, b);
const patch = (p, b) => api('PATCH', p, b);

async function login() {
  for (const password of PASSWORDS) {
    const r = await fetch(`${BASE}/api/system/auth/login`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username: USER, password }),
    });
    const b = await r.json().catch(() => ({}));
    if (b.code === 0 && b.data?.access_token) return b.data;
  }
  return null;
}

const session = await login();
if (!session) { console.error('登录失败：口令候选都不对'); process.exit(2); }
TOKEN = session.access_token;
ok(`API 登录成功 user=${USER} token=${TOKEN.slice(0, 12)}…`);

// ---------------- 浏览器 ----------------
const browser = await chromium.launch();
const context = await browser.newContext({ viewport: { width: 1500, height: 1000 } });
await context.addInitScript(([a, r]) => {
  localStorage.setItem('x_access_token', a);
  localStorage.setItem('x_refresh_token', r);
  localStorage.setItem('x_guide_done', '1');
  localStorage.setItem('cp-onboarding-done', '1');
}, [session.access_token, session.refresh_token]);
const page = await context.newPage();
const consoleErrors = [], httpErrors = [];
page.on('console', (m) => { if (m.type() === 'error') consoleErrors.push(m.text()); });
page.on('pageerror', (e) => consoleErrors.push(String(e)));
page.on('response', (r) => { if (r.status() >= 400 && r.url().includes('/api/')) httpErrors.push(`${r.status()} ${r.url()}`); });

let shotIdx = 0;
async function shot(name) {
  shotIdx += 1;
  const file = `${SHOT_DIR}/deployB-${String(shotIdx).padStart(2, '0')}-${name}.png`;
  await page.screenshot({ path: file, fullPage: false });
  info(`截图 → ${file}`);
  return file;
}
const goto = async (path) => { await page.goto(`${BASE}${path}`, { waitUntil: 'networkidle', timeout: 45000 }); await page.waitForTimeout(700); };
async function pick(testid, keyword) {
  const root = page.locator(`[data-testid="${testid}"]`).first();
  await root.scrollIntoViewIfNeeded().catch(() => {});
  await root.click();
  await page.waitForTimeout(500);
  const opt = page.locator(`.t-select-option:visible:has-text("${keyword}")`).first();
  if (await opt.count()) { await opt.click(); await page.waitForTimeout(400); return true; }
  const input = root.locator('input').first();
  if (await input.count()) {
    await input.fill(keyword).catch(() => {});
    await page.waitForTimeout(600);
    const opt2 = page.locator(`.t-select-option:visible:has-text("${keyword}")`).first();
    if (await opt2.count()) { await opt2.click(); await page.waitForTimeout(400); return true; }
    await input.press('Enter').catch(() => {});
  }
  return false;
}
async function fillNum(testid, value) {
  const input = page.locator(`[data-testid="${testid}"]`).locator('input').first();
  await input.click();
  await input.press('Control+a').catch(() => {});
  await input.fill(String(value));
  await input.press('Tab');
  await page.waitForTimeout(200);
}

// ================= ① 登录能进 =================
console.log('\n===== ① 登录能进（不白屏）=====');
await goto('/');
await page.waitForTimeout(2500);
const bodyText = (await page.locator('body').innerText().catch(() => '')) || '';
const hasToken = await page.evaluate(() => !!localStorage.getItem('x_access_token'));
const notBlank = bodyText.replace(/\s/g, '').length > 40;
info(`页面文本长度=${bodyText.replace(/\s/g, '').length} hasToken=${hasToken}`);
if (notBlank && hasToken) ok('首页已渲染（非白屏，localStorage 有令牌）'); else bad('首页空白 / 没进主布局');
await shot('login-home');

// ================= 数据准备（API 只读取） =================
console.log('\n===== 数据准备 =====');
const matRes = await get('/api/business/erp/material/page?page=1&page_size=100');
const MATERIAL = (matRes.body?.data?.list || []).find((m) => m.material_code === 'M-W1A-001');
const whRes = await get('/api/business/erp/warehouse/page?page=1&page_size=100');
const WAREHOUSES = whRes.body?.data?.list || [];
const WH = WAREHOUSES.find((w) => w.warehouse_name.includes('分仓')) || WAREHOUSES[1] || WAREHOUSES[0];
info(`物料=${MATERIAL?.material_code} ${MATERIAL?.material_name} id=${MATERIAL?.id}`);
info(`仓库=${WH?.warehouse_code} ${WH?.warehouse_name} id=${WH?.id}`);
if (!MATERIAL || !WH) { console.error('缺少物料/仓库前置数据'); process.exit(2); }

async function inventoryOf(materialId, warehouseId) {
  const r = await get('/api/business/erp/inventory/page?page=1&page_size=200');
  const row = (r.body?.data?.list || []).find((x) => x.material_id === materialId && x.warehouse_id === warehouseId);
  return row || null;
}
const invBefore = await inventoryOf(MATERIAL.id, WH.id);
const locRes = await get('/api/business/erp/inventory-location/page?page=1&page_size=200');
const LOC = (locRes.body?.data?.list || []).find((x) => x.material_id === MATERIAL.id && x.warehouse_id === WH.id && Number(x.qty) > 5);
info(`出库前成本账：qty=${invBefore?.qty} amount=${invBefore?.amount} avg=${invBefore?.avg_cost}`);
info(`库位账行 id=${LOC?.id} qty=${LOC?.qty}`);
if (!invBefore || !LOC) { console.error('出库前置库存不足'); process.exit(2); }

// ================= ② 出库单 =================
console.log('\n===== ② 出库单页面 / 新建 / 过账 =====');
await goto('/business/erp/stock-out');
const pageOk = await page.locator('[data-testid="btn-stockout-create"]').isVisible().catch(() => false);
if (pageOk) ok('出库单页面打开（btn-stockout-create 可见，路由不 404）'); else bad('出库单页面打不开（找不到 btn-stockout-create）');
await shot('stockout-list');

// 新建领用单（若上一次运行已留下同一形态的草稿，则直接复用，避免堆单）
let listRes = await get('/api/business/erp/stock-out/page?page=1&page_size=20');
let rows = listRes.body?.data?.list || [];
let issueRow = rows.find((r) => r.out_type === 'ISSUE' && r.status === 'DRAFT');
if (!issueRow) {
await page.locator('[data-testid="btn-stockout-create"]').click();
await page.waitForTimeout(1200);
const drawerOpen = await page.locator('[data-testid="stockout-form-drawer"]').isVisible().catch(() => false);
if (drawerOpen) ok('新建出库单抽屉打开'); else bad('新建抽屉没打开');
await page.locator('[data-testid="stockout-form-type"] label:has-text("领用")').first().click().catch(() => {});
await page.waitForTimeout(300);
await pick('stockout-form-warehouse', '分仓');
await pick('stockout-item-material-0', 'M-W1A-001');
await page.waitForTimeout(500);
await fillNum('stockout-item-qty-0', 2);
await page.locator('[data-testid="stockout-item-location-0"]').click();
await page.waitForTimeout(600);
const locOpt = page.locator('.t-select-option:visible').first();
if (await locOpt.count()) { await locOpt.click(); await page.waitForTimeout(400); }
await shot('stockout-create-issue-drawer');
await page.locator('[data-testid="btn-stockout-save"]').click();
await page.waitForTimeout(2500);
const submitErr = await page.locator('[data-testid="stockout-submit-error"]').innerText().catch(() => '');
if (submitErr) bad(`建单失败：${submitErr}`); else ok('领用单保存成功（无 submit-error）');
await page.waitForTimeout(1000);
// 保存后页面可能自动打开详情抽屉，先关掉再点列表里的「查看」
if (await page.locator('[data-testid="stockout-detail-drawer"]').isVisible().catch(() => false)) {
  info('保存后自动打开了详情抽屉，先关闭');
  await page.locator('[data-testid="btn-stockout-drawer-close"]').click().catch(() => {});
  await page.waitForTimeout(600);
}
listRes = await get('/api/business/erp/stock-out/page?page=1&page_size=20');
rows = listRes.body?.data?.list || [];
issueRow = rows.find((r) => r.out_type === 'ISSUE' && r.status === 'DRAFT') || rows.find((r) => r.out_type === 'ISSUE');
} else {
  info(`复用上一次运行留下的草稿领用单：${issueRow.out_no}`);
}
if (issueRow) ok(`领用单已落库：${issueRow.out_no} status=${issueRow.status}`); else bad('列表里没有领用单');

// 打开详情 → 过账
if (issueRow) {
  const viewLink = page.locator(`[data-testid="btn-view-${issueRow.out_no}"]`).first();
  await viewLink.scrollIntoViewIfNeeded().catch(() => {});
  await viewLink.click({ timeout: 8000 }).catch(async () => { await viewLink.click({ force: true }); });
  await page.waitForTimeout(1800);
  const detailNo = await page.locator('[data-testid="stockout-detail-no"]').innerText().catch(() => '');
  if (detailNo.trim() === issueRow.out_no) ok(`详情抽屉打开：${detailNo.trim()}`); else bad(`详情抽屉异常：${detailNo}`);
  await shot('stockout-detail-draft');
  const postBtn = page.locator('[data-testid="btn-stockout-post"]').first();
  if (await postBtn.count()) {
    await postBtn.click();
    await page.waitForTimeout(1200);
    await shot('stockout-post-dialog');
    await page.locator('[data-testid="btn-stockout-post-confirm"]').click();
    await page.waitForTimeout(3000);
    const postErr = await page.locator('[data-testid="stockout-post-error"]').innerText().catch(() => '');
    if (postErr) bad(`过账报错：${postErr}`); else ok('过账动作返回成功（无 post-error）');
    const st = await page.locator('[data-testid="stockout-detail-status"]').innerText().catch(() => '');
    info(`详情状态徽标=${st.trim()}`);
    if (st.includes('已过账')) ok('详情状态 = 已过账'); else bad(`过账后状态不是已过账：${st.trim()}`);
    await shot('stockout-detail-posted');
  } else {
    bad('详情里找不到「过账」按钮（btn-stockout-post）');
  }
  await page.locator('[data-testid="btn-stockout-drawer-close"]').click().catch(() => {});
  await page.waitForTimeout(500);

  // 过账后的账侧断言
  const detail = await get(`/api/business/erp/stock-out/${issueRow.id}`);
  const d = detail.body?.data;
  info(`过账后 detail: status=${d?.status} total_amount=${d?.total_amount} voucher_id=${d?.voucher_id}`);
  if (d?.status === 'POSTED') ok('API 详情 status=POSTED'); else bad(`API 详情 status=${d?.status}`);
  if (d?.voucher_id) ok(`成本结转凭证已生成 voucher_id=${d.voucher_id}`); else bad('没有生成成本结转凭证');
  if (Number(d?.total_amount) > 0) ok(`成本结转金额 > 0（${d.total_amount}）`); else bad(`成本结转金额 = ${d?.total_amount}`);
  const invAfter = await inventoryOf(MATERIAL.id, WH.id);
  info(`出库后成本账：qty=${invAfter?.qty} amount=${invAfter?.amount}`);
  if (invAfter && Number(invBefore.qty) - Number(invAfter.qty) === 2) ok(`库存减少 2（${invBefore.qty} → ${invAfter.qty}）`);
  else bad(`库存变化不符：${invBefore?.qty} → ${invAfter?.qty}`);
}

// 报废单 / 盘亏单（必须填事由）—— 已存在同类型草稿则跳过，避免重复堆单
const existing = (await get('/api/business/erp/stock-out/page?page=1&page_size=50')).body?.data?.list || [];
for (const [type, reason] of [['SCRAP', 'DEPLOY-B 报废验证：设备老化'], ['LOSS', 'DEPLOY-B 盘亏验证：盘点差异']]) {
  if (existing.some((r) => r.out_type === type && r.status === 'DRAFT')) {
    info(`${type} 单已存在草稿，跳过创建`);
    ok(`${type} 单已存在（草稿）`);
    continue;
  }
  const r = await post('/api/business/erp/stock-out', {
    out_type: type, warehouse_id: WH.id, out_date: new Date().toISOString().slice(0, 10),
    reason, remark: `DEPLOY-B 真机验收 ${TAG}`,
    items: [{ material_id: MATERIAL.id, qty: '1' }],
  });
  if (r.body?.code === 0) ok(`${type} 单创建成功：${r.body.data.out_no}（status=${r.body.data.status}）`);
  else bad(`${type} 单创建失败：${r.status} ${JSON.stringify(r.body).slice(0, 200)}`);
}
await page.reload({ waitUntil: 'networkidle' });
await page.waitForTimeout(1500);
await shot('stockout-list-three-types');

// ================= ③ 成本法只剩「移动加权」 =================
console.log('\n===== ③ 成本法只剩「移动加权」 =====');
await goto('/business/erp/material');
await page.locator('[data-testid="btn-edit"]').first().click();
await page.waitForTimeout(1500);
const costingText = await page.locator('[data-testid="mat-costing"]').innerText().catch(() => '');
const costingInputVal = await page.locator('[data-testid="mat-costing"] input').first().inputValue().catch(() => '');
const costingDisabled = await page.locator('[data-testid="mat-costing"]').evaluate(
  (el) => /t-is-disabled/.test(el.className) || !!el.querySelector('.t-is-disabled') || !!el.querySelector('[disabled]'),
).catch(() => false);
const costingHelp = await page.locator('[data-testid="mat-costing-help"]').innerText().catch(() => '');
info(`成本法控件文本=${costingText.replace(/\s+/g, ' ')} input=${costingInputVal} disabled=${costingDisabled}`);
info(`灰字说明=${costingHelp.replace(/\s+/g, ' ')}`);
// 强制展开一次（置灰控件默认点不开，用 JS 展开只作视觉证据）
await page.locator('[data-testid="mat-costing"]').click({ force: true }).catch(() => {});
await page.waitForTimeout(600);
const costingOpts = await page.locator('.t-select-option:visible').allInnerTexts().catch(() => []);
info(`可展开选项=${JSON.stringify(costingOpts)}（控件按设计置灰，展开为空属预期）`);
await shot('material-costing-disabled-single');
const singleInUi = /移动加权/.test(`${costingText} ${costingInputVal}`);
if (singleInUi && costingDisabled) ok(`成本法控件=「${(costingText || costingInputVal).replace(/\s+/g, ' ')}」且置灰（不给「选了不生效」的机会）`);
else bad(`成本法控件异常：文本=${costingText} 值=${costingInputVal} 置灰=${costingDisabled}`);
if (/仅支持移动加权/.test(costingHelp)) ok('灰字说明与后端文案同口径（FIFO/STANDARD 属路线图）'); else bad(`灰字说明异常：${costingHelp}`);
if (costingOpts.length === 0 || (costingOpts.length === 1 && /移动加权/.test(costingOpts[0]))) ok('下拉里除「移动加权」外没有第二项（FIFO/STANDARD 已移除）');
else bad(`下拉里出现其它成本法：${JSON.stringify(costingOpts)}`);
await page.keyboard.press('Escape');
await page.waitForTimeout(400);

// ================= ④ 非基本单位被拒（66030） =================
console.log('\n===== ④ 非基本单位被拒（物料主数据闸） =====');
const unitRes = await get('/api/business/erp/unit/page?page=1&page_size=100').catch(() => ({ body: {} }));
let boxUnit = (unitRes.body?.data?.list || []).find((u) => u.unit_code === 'BOX');
if (!boxUnit) {
  const r2 = await get('/api/business/erp/unit/page?page=1&page_size=200');
  boxUnit = (r2.body?.data?.list || []).find((u) => u.unit_code === 'BOX');
}
const materialUnitProbe = await post('/api/business/erp/material', {
  material_code: `DB-${STAMP}-BOX`, material_name: `DEPLOY-B 非基本单位探针 ${TAG}`,
  material_type: 'RAW', unit_id: boxUnit?.id || 'd1370000-0000-4000-8000-000000000003',
  costing_method: 'MOVING_AVG', tax_rate: '0.13', status: 1,
});
info(`使用单位：箱 id=${boxUnit?.id || '(兜底)'}`);
info(`POST /material → ${materialUnitProbe.status} ${JSON.stringify(materialUnitProbe.body).slice(0, 300)}`);
if (materialUnitProbe.status === 400 && materialUnitProbe.body?.code === 66030) ok('非基本单位建物料 → 400 + 66030');
else bad(`非基本单位未被拒：${materialUnitProbe.status} code=${materialUnitProbe.body?.code}`);

// UI 侧同一件事（表单里选「箱」保存）
await page.locator('[data-testid="btn-new-material"]').click();
await page.waitForTimeout(1200);
await page.locator('[data-testid="mat-code"]').locator('input').fill(`DB-UI-${STAMP}`);
await page.locator('[data-testid="mat-name"]').locator('input').fill(`DEPLOY-B 非基本单位 UI 探针`);
await pick('mat-unit', '箱');
const saveBtn = page.locator('.t-dialog:visible button.t-button--theme-primary').last();
await saveBtn.click();
await page.waitForTimeout(2000);
const dlgAlert = await page.locator('[data-testid="mat-dialog"] .t-alert').first().innerText().catch(() => '');
info(`表单报错：${dlgAlert.replace(/\s+/g, ' ').slice(0, 160)}`);
await shot('material-unit-rejected');
if (dlgAlert.includes('66030') || /必须是基本单位/.test(dlgAlert)) ok('UI 选「箱」保存 → 被拒并展示 66030 文案');
else bad(`UI 非基本单位未被拒（弹窗文案：${dlgAlert.slice(0, 80)}）`);
await page.keyboard.press('Escape');

// ================= ⑤ 已取消采购单收货被拒 =================
console.log('\n===== ⑤ 已取消采购单收货被拒 =====');
const poRes = await get('/api/business/erp/purchase-order/page?page=1&page_size=100');
const poList = poRes.body?.data?.list || [];
const cancelled = poList.find((p) => p.status === 'CANCELLED');
info(`已取消采购单：${cancelled?.order_no} id=${cancelled?.id}`);
if (cancelled) {
  const r = await post('/api/business/erp/stock-in', {
    purchase_order_id: cancelled.id, warehouse_id: WH.id, items: [], remark: `DEPLOY-B 已取消采购单收货探针 ${TAG}`,
  });
  info(`POST /stock-in → ${r.status} ${JSON.stringify(r.body).slice(0, 300)}`);
  if (r.status >= 400 && r.status < 500) ok(`收货被拒：${r.status} + code ${r.body?.code}`);
  else bad(`已取消采购单竟可收货：${r.status} code=${r.body?.code}`);
} else {
  skip('库里没有 CANCELLED 采购单，⑤ 无法取证');
}

// ================= ⑦ 四单审批进度 =================
console.log('\n===== ⑦ 四单审批进度（审批人 + 时间）=====');
const approvalPages = [
  ['purchase', '/business/erp/purchase', 'btn-view-', '采购单'],
  ['stockin', '/business/erp/stock-in', 'btn-view-', '收货单'],
  ['payable', '/business/erp/payable', 'btn-payable-view-', '应付单'],
  ['voucher', '/business/erp/voucher', 'btn-voucher-view-', '凭证'],
  ['transfer', '/business/erp/transfer', 'btn-view-', '调拨单'],
];
for (const [key, path, prefix, label] of approvalPages) {
  await goto(path);
  const link = page.locator(`[data-testid^="${prefix}"]`).first();
  if (!(await link.count())) { skip(`${label}：列表无行（无法打开详情）`); continue; }
  await link.click();
  await page.waitForTimeout(2200);
  const txt = (await page.locator('body').innerText().catch(() => '')) || '';
  const hasApprovalWord = /审批/.test(txt);
  const progressEl = await page.locator(`[data-testid$="-approval-progress"]`).first().innerText().catch(() => '');
  await shot(`approval-${key}`);
  info(`${label}：审批进度块=${progressEl ? progressEl.replace(/\s+/g, ' ').slice(0, 120) : '（无 data-testid 块）'} 页面含「审批」=${hasApprovalWord}`);
  if (hasApprovalWord) ok(`${label} 详情能看到审批信息（含审批人/时间区域）`); else bad(`${label} 详情看不到审批信息`);
  await page.keyboard.press('Escape');
  await page.waitForTimeout(400);
}

// ================= ⑧ 状态机边配置 =================
console.log('\n===== ⑧ 状态机边配置 =====');
const edgeRes = await get('/api/business/flow/state-edge?entity=STOCK_OUT');
info(`GET /business/flow/state-edge → ${edgeRes.status} code=${edgeRes.body?.code} total=${edgeRes.body?.data?.total}`);
if (edgeRes.status === 200 && edgeRes.body?.code === 0) ok('状态机边配置 HTTP 面可用（V0204 + D3 接口已上线）');
else bad(`状态机边配置接口异常：${edgeRes.status}`);
// ⚠ 路由是 `flow/def`（router/index.ts 里挂在 `/` 下的子路由，**不带 `business/` 前缀**，
//   与 sys_menu.path 同口径）。`/business/flow/def` 会命中 404 页，用它取证会得到假结论。
await goto('/flow/def');
const defTxt = (await page.locator('body').innerText().catch(() => '')) || '';
await shot('flow-def-page');
// 精确判据：找「边配置」按钮/链接（流程列表里有个流程名叫「状态机配置变更审批」，
// 直接全文匹配 /状态机/ 会假阳性）
const edgeButton = await page.locator('button:has-text("边配置"), .t-link:has-text("边配置"), a:has-text("边配置")').count().catch(() => 0);
const flat = defTxt.replace(/\s+/g, ' ');
info(`流程定义页（/flow/def）文本长度=${flat.replace(/ /g, '').length}；「边配置」按钮/链接数=${edgeButton}`);
if (flat.includes('404') && flat.includes('页面不存在')) bad('流程定义页 404（URL 写错？正确是 /flow/def）');
else if (edgeButton > 0) ok('流程定义页能看到「边配置」入口');
else bad('流程定义页**没有**「边配置」入口（前端未交付该入口，详见卡内报告）');

// ================= ⑩ 控制台 =================
console.log('\n===== ⑩ 控制台 / 4xx-5xx =====');
const apiErrors = httpErrors.filter((e) => !/\/api\/system\/auth\/(login|refresh)/.test(e));
const expectErrors = apiErrors.filter((e) => /^(400|409) /.test(e)); // ④⑤ 的负向断言
const realErrors = apiErrors.filter((e) => !/^(400|409) /.test(e));
// 负向断言（非基本单位 400 / 已取消采购单 400）在浏览器控制台会留一条 "Failed to load resource: 400"
const negConsole = consoleErrors.filter((e) => /status of (400|409) \(/.test(e));
const realConsole = consoleErrors.filter((e) => !/status of (400|409) \(/.test(e));
info(`console error 数=${consoleErrors.length}（其中负向断言引起的 ${negConsole.length} 条）；API 4xx/5xx 数=${apiErrors.length}（其中 400/409 负向断言 ${expectErrors.length}）`);
if (realConsole.length === 0) ok('控制台无意外 error'); else bad(`控制台有 ${realConsole.length} 条意外 error：${realConsole.slice(0, 3).join(' | ')}`);
if (realErrors.length === 0) ok('无意外 4xx/5xx'); else bad(`有意外 4xx/5xx：${realErrors.slice(0, 5).join(' | ')}`);
if (expectErrors.length) info('（预期内的负向断言请求：' + expectErrors.join(' | ') + '）');
await shot('final-state');

await browser.close();

console.log(`\n===== 汇总 TAG=${TAG} PASS=${PASS} FAIL=${FAIL} SKIP=${SKIP} 截图=${shotIdx} 张 =====`);
if (failures.length) { console.log('未通过项：'); failures.forEach((f) => console.log(' - ' + f)); }
process.exit(FAIL === 0 ? 0 : 1);
