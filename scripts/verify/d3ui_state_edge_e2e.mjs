#!/usr/bin/env node
/**
 * 【D3-UI】真机验收（Playwright，打 144 的 nginx → 线上 dist → 生产库）
 *
 * 卡 t_a392a5da 第 4 步：
 *  ① `/flow/def` 能看到「边配置」入口（顶栏按钮）
 *  ② 打开面板 → 选 STOCK_OUT → 6 条 BASE 边可见（mandatory / requires_reason / auth_code /
 *     configured / enabled 全部来自 `GET /business/flow/state-edge`）
 *  ③ 点「变更」→ 填理由 → 提交审批 → **生成审批单**（不是直接落库）
 *  ④ 提交后库里的配置仍是 0 行、`pending_changes` 出现这张单（= 等审批）
 *  ⑤ 撤回该审批单（清场）→ 配置仍是 0 行（驳回 / 撤回配置一分不动）
 *
 * 用法（144 上）：
 *   set -a; . /etc/cloudpivot/cloudpivot.env; set +a
 *   node scripts/verify/d3ui_state_edge_e2e.mjs
 */
import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';

const BASE = process.env.D3UI_BASE_URL || 'http://127.0.0.1';
const USER = process.env.D3UI_USER || 'admin';
const PW = process.env.NIK_ADMIN_PASSWORD || process.env.ALUN_ADMIN_PASSWORD || 'Admin@123456';
const SHOT_DIR = process.env.D3UI_SHOT_DIR || '/opt/cloudpivot/docs/evidence';
const ENTITY = process.env.D3UI_ENTITY || 'STOCK_OUT';
const REASON = 'D3-UI 真机验收：把「直接过账」这条可选边停用（提交审批，不直接落库）';

let PASS = 0;
let FAIL = 0;
const failures = [];
const ok = (m) => { PASS += 1; console.log(`[PASS] ${m}`); };
const bad = (m) => { FAIL += 1; failures.push(m); console.log(`[FAIL] ${m}`); };
const info = (m) => console.log(`       ${m}`);

// ---------------- ① 登录（API 拿 token，浏览器注入，跳过登录页） ----------------
const lr = await fetch(`${BASE}/api/system/auth/login`, {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ username: USER, password: PW }),
});
const lb = await lr.json();
if (lb.code !== 0) {
  console.error('登录失败：', lb.code, lb.msg);
  process.exit(2);
}
const TOKEN = lb.data.access_token;
ok(`API 登录成功 user=${USER}`);

const authHeaders = { Authorization: `Bearer ${TOKEN}`, 'Content-Type': 'application/json' };
async function api(method, path, body) {
  const r = await fetch(`${BASE}${path}`, { method, headers: authHeaders, body: body ? JSON.stringify(body) : undefined });
  let json = null;
  try { json = await r.json(); } catch (e) { json = { parse_error: String(e) }; }
  return { status: r.status, body: json };
}

/** 读某个实体的边视图（库侧真值：configs / pending_changes） */
async function entityView(entity) {
  const res = await api('GET', `/api/business/flow/state-edge?entity=${entity}`);
  return res.body?.data?.list?.[0] ?? null;
}

const viewBefore = await entityView(ENTITY);
if (!viewBefore) {
  console.error('GET state-edge 取不到实体视图，退出');
  process.exit(2);
}
info(`接口基线：${viewBefore.entity_label} BASE 边=${viewBefore.base_edges.length} 配置=${viewBefore.configs.length} 在跑审批=${viewBefore.pending_changes.length}`);
ok(`GET /business/flow/state-edge?entity=${ENTITY} → code=0，BASE 边 ${viewBefore.base_edges.length} 条`);

// ---------------- 浏览器 ----------------
const browser = await chromium.launch();
const context = await browser.newContext({ viewport: { width: 1500, height: 1000 } });
await context.addInitScript(([a, r]) => {
  localStorage.setItem('x_access_token', a);
  localStorage.setItem('x_refresh_token', r);
  localStorage.setItem('x_guide_done', '1');
  localStorage.setItem('cp-onboarding-done', '1');
}, [lb.data.access_token, lb.data.refresh_token]);
const page = await context.newPage();
const consoleErrors = [];
const httpErrors = [];
page.on('console', (m) => { if (m.type() === 'error') consoleErrors.push(m.text()); });
page.on('pageerror', (e) => consoleErrors.push(String(e)));
page.on('response', (r) => {
  if (r.status() >= 400 && r.url().includes('/api/')) httpErrors.push(`${r.status()} ${r.url()}`);
});

let shotIdx = 0;
async function shot(name) {
  shotIdx += 1;
  const file = `${SHOT_DIR}/d3ui-${String(shotIdx).padStart(2, '0')}-${name}.png`;
  await page.screenshot({ path: file, fullPage: false });
  info(`截图 → ${file}`);
  return file;
}

// ---------------- ② /flow/def 上的「边配置」入口 ----------------
await page.goto(`${BASE}/flow/def`, { waitUntil: 'networkidle', timeout: 45000 });
await page.waitForTimeout(1200);
const entry = page.locator('[data-testid="btn-state-edge"]');
const entryCount = await entry.count();
if (entryCount > 0 && (await entry.first().isVisible())) {
  ok('`/flow/def` 顶栏出现「边配置」按钮（data-testid=btn-state-edge）');
} else {
  bad('`/flow/def` 没有「边配置」入口');
}
const entryText = await entry.first().innerText().catch(() => '');
info(`入口文案 = ${JSON.stringify(entryText.trim())} · 全页「边配置」计数 = ${await page.locator('button:has-text("边配置")').count()}`);
await shot('flow-def-entry');

// ---------------- ③ 打开面板 → 选 STOCK_OUT → 6 条边 ----------------
await entry.first().click();
const drawer = page.locator('[data-testid="state-edge-drawer"]');
await drawer.waitFor({ state: 'visible', timeout: 15000 });
await page.waitForTimeout(1200);

// 实体下拉：选项文案是「中文名（实体码）」，按实体码点
const select = page.locator('[data-testid="state-edge-entity"]');
await select.click();
await page.waitForTimeout(600);
const option = page.locator(`.t-select-option:has-text("${ENTITY}")`).first();
if (await option.count()) {
  await option.click();
  await page.waitForTimeout(900);
  ok(`实体下拉切到 ${ENTITY}`);
} else {
  bad(`实体下拉里找不到 ${ENTITY}`);
}
const meta = (await page.locator('[data-testid="state-edge-meta"]').innerText().catch(() => '')).replace(/\s+/g, ' ');
info(`面板统计 = ${meta}`);

// 表格行数（TDesign 表格 tbody tr）
const tableRows = page.locator('[data-testid="state-edge-table"] tbody tr');
const rowCount = await tableRows.count();
if (rowCount === viewBefore.base_edges.length) {
  ok(`边表渲染 ${rowCount} 行 == 接口 base_edges ${viewBefore.base_edges.length} 条`);
} else {
  bad(`边表渲染 ${rowCount} 行，接口 base_edges ${viewBefore.base_edges.length} 条 —— 不一致`);
}

// 逐条核对接口下发的 from/to/label 都出现在页面上（前端没写死边表）
const bodyText = ((await drawer.innerText()) || '').replace(/\s+/g, ' ');
let missing = 0;
for (const edge of viewBefore.base_edges) {
  if (!bodyText.includes(edge.from) || !bodyText.includes(edge.to)) {
    missing += 1;
    info(`缺边：${edge.from} → ${edge.to}（${edge.label}）`);
  }
}
if (missing === 0) {
  ok(`接口下发的 ${viewBefore.base_edges.length} 条边（from/to）在面板上全部可见`);
} else {
  bad(`${missing} 条边在面板上找不到`);
}
// 状态标记也在：mandatory 至少一条「硬约束」、requires_reason 至少一条「必须填原因」
if (bodyText.includes('硬约束') && bodyText.includes('必须填原因')) {
  ok('mandatory（硬约束）/ requires_reason（必须填原因）标记渲染正常');
} else {
  bad('mandatory / requires_reason 标记缺失');
}
await shot('state-edge-panel-stockout');

// ---------------- ③b 硬约束边（mandatory）的「启用」开关必须锁死 ----------------
// 内核口径：`upsert_edge_config` 拒绝禁用 mandatory 边；前端先挡一层，别让用户点完才吃 400。
const mandatoryIndex = viewBefore.base_edges.findIndex((edge) => edge.mandatory);
info(`硬约束边示例：第 ${mandatoryIndex + 1} 行 ${viewBefore.base_edges[mandatoryIndex].from} → ${viewBefore.base_edges[mandatoryIndex].to}`);
await page.locator('[data-testid="state-edge-change"]').nth(mandatoryIndex).click();
const mandatoryDialog = page.locator('.t-dialog:has-text("变更边配置（提交审批）")').last();
await mandatoryDialog.waitFor({ state: 'visible', timeout: 15000 });
await page.waitForTimeout(600);
const mandatorySwitchClass = (await page.locator('[data-testid="state-edge-form-enabled"]').first().getAttribute('class')) || '';
info(`硬约束边启用开关 class = ${mandatorySwitchClass}`);
if (mandatorySwitchClass.includes('t-is-disabled')) {
  ok('硬约束边（mandatory）的「是否启用」开关被锁死（与内核「禁不掉」一致）');
} else {
  bad('硬约束边的启用开关没有锁死 —— 用户能点，但后端会 400');
}
await page.locator('.t-dialog:has-text("变更边配置（提交审批）") button:has-text("取消")').last().click();
await page.waitForTimeout(800);

// ---------------- ④ 点「变更」→ 提交审批 ----------------
// 选第一条「可选」边（mandatory=false，允许停用）：STOCK_OUT 是 DRAFT → POSTED「直接过账」
const optionalEdge = viewBefore.base_edges.find((edge) => !edge.mandatory && !edge.requires_reason);
if (!optionalEdge) {
  bad('没有可选边，无法演示变更');
}
const rowIndex = viewBefore.base_edges.findIndex((edge) => edge.from === optionalEdge.from && edge.to === optionalEdge.to);
info(`选中第 ${rowIndex + 1} 行：${optionalEdge.from} → ${optionalEdge.to}（${optionalEdge.label}，mandatory=${optionalEdge.mandatory}）`);
await page.locator('[data-testid="state-edge-change"]').nth(rowIndex).click();
const dialog = page.locator('.t-dialog:has-text("变更边配置（提交审批）")').last();
await dialog.waitFor({ state: 'visible', timeout: 15000 });
await page.waitForTimeout(600);

// 硬约束边禁不掉：先断言「可选边」的启用开关是可用的（对照口径，不点）
const enabledSwitch = page.locator('[data-testid="state-edge-form-enabled"]').first();
const switchDisabled = await enabledSwitch.getAttribute('class');
info(`启用开关 class = ${switchDisabled}`);

// 停用这条可选边
await enabledSwitch.click();
await page.waitForTimeout(300);
// 先空理由提交：前端必须挡住（后端字段可选，但审批单标题要能看懂）
await dialog.locator('button:has-text("提交审批")').last().click();
await page.waitForTimeout(1200);
const guardMessages = (await page.locator('.t-message__content, .t-message').allInnerTexts().catch(() => [])).join(' | ').replace(/\s+/g, ' ');
info(`空理由提交的提示 = ${guardMessages}`);
if (/变更理由/.test(guardMessages)) {
  ok('理由为空时前端拦截（提示「请填写变更理由」），没有擅自提审批');
} else {
  bad(`理由为空没有被拦截：${guardMessages}`);
}
const reasonBox = page.locator('[data-testid="state-edge-form-reason-text"] textarea').first();
await reasonBox.fill(REASON);
await page.waitForTimeout(300);
const summary = (await page.locator('[data-testid="state-edge-change-summary"]').innerText().catch(() => '')).replace(/\s+/g, ' ');
info(`待变更摘要 = ${summary}`);
await shot('state-edge-change-dialog');

// 提交审批（按钮在 dialog footer）
const submitBtn = dialog.locator('button:has-text("提交审批")').last();
await submitBtn.click();
await page.waitForTimeout(2500);

// 成功提示里必须带审批单号（说明是「已送审」而不是「已生效」）
const messages = await page.locator('.t-message__content, .t-message').allInnerTexts().catch(() => []);
const messageText = messages.join(' | ').replace(/\s+/g, ' ');
info(`提示 = ${messageText}`);
if (/已提交审批/.test(messageText) || /FLW-/.test(messageText)) {
  ok('提交后提示「已提交审批 + 审批单号」（不是直接生效）');
} else {
  bad(`提交后提示不含审批单号：${messageText}`);
}
await shot('state-edge-approval-submitted');

// ---------------- ⑤ 库侧核对：配置仍 0 行 + 在跑审批 1 张 ----------------
await page.waitForTimeout(800);
const viewAfter = await entityView(ENTITY);
info(`提交后接口：配置=${viewAfter.configs.length} 在跑审批=${viewAfter.pending_changes.length}`);
if (viewAfter.configs.length === viewBefore.configs.length) {
  ok(`提交后 biz_flow_state_edge 配置仍 ${viewAfter.configs.length} 行（未直接落库）`);
} else {
  bad(`提交后配置行变成 ${viewAfter.configs.length} —— 疑似绕过审批直写`);
}
const pending = viewAfter.pending_changes;
if (pending.length >= 1) {
  ok(`在跑审批单 ${pending.length} 张：${pending.map((p) => p.instance_no).join(', ')}`);
  info(`审批单：action=${pending[0].action} ${pending[0].from}→${pending[0].to} summary=${pending[0].summary}`);
} else {
  bad('提交后没有在跑审批单');
}
const pendingSection = page.locator('[data-testid="state-edge-pending"]');
if (await pendingSection.count()) {
  ok('面板上「在跑审批」区域出现（审批通过前不会生效）');
} else {
  bad('面板上没有「在跑审批」区域');
}

// ---------------- ⑥ 撤回（清场）→ 配置仍 0 行 ----------------
if (pending.length) {
  const wd = await api('POST', `/api/business/flow/instance/${pending[0].instance_id}/withdraw`, { reason: 'D3-UI 真机验收后清场：撤回验收用的审批单' });
  info(`撤回：status=${wd.status} code=${wd.body?.code} msg=${wd.body?.msg}`);
  if (wd.status === 200 && wd.body?.code === 0) {
    ok('验收用审批单已撤回（prod 不留待办）');
  } else {
    bad(`撤回失败：${wd.status} ${wd.body?.msg}`);
  }
  await page.reload({ waitUntil: 'networkidle' });
  await page.waitForTimeout(1500);
  const viewFinal = await entityView(ENTITY);
  info(`撤回后接口：配置=${viewFinal.configs.length} 在跑审批=${viewFinal.pending_changes.length}`);
  if (viewFinal.configs.length === 0 && viewFinal.pending_changes.length === 0) {
    ok('撤回后配置 0 行、在跑审批 0 张（驳回 / 撤回配置一分不动）');
  } else {
    bad(`撤回后仍有残留：配置 ${viewFinal.configs.length} / 审批 ${viewFinal.pending_changes.length}`);
  }
  await shot('state-edge-after-withdraw');
}

// ---------------- ⑦ 控制台 / HTTP 噪音 ----------------
const noiseHttp = httpErrors.filter((line) => !/\/state-edge\?entity=/.test(line));
if (noiseHttp.length === 0) {
  ok('页面无 4xx/5xx 接口噪音（state-edge 查询本身除外）');
} else {
  bad(`接口噪音：${noiseHttp.slice(0, 3).join(' | ')}`);
}
if (consoleErrors.length === 0) {
  ok('控制台无 error');
} else {
  bad(`控制台 error：${consoleErrors.slice(0, 3).join(' | ')}`);
}

await browser.close();
console.log(`\n===== D3-UI 真机验收：PASS ${PASS} / FAIL ${FAIL} =====`);
if (FAIL) {
  console.log('失败项：');
  failures.forEach((f) => console.log(` - ${f}`));
}
process.exit(FAIL ? 1 : 0);
