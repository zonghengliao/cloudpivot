#!/usr/bin/env node
/**
 * ERP-B2 页面验收：**支撑功能 / 参数配置** 页要多出一组「ERP 内控」，四条开关的状态与库一致。
 *
 * 验的是卡面设计自检表 F6「状态可见」：管理员打开参数页就能看见开关、看得懂当前是开还是关。
 *
 * 前置：后端隔离实例（erpb2_e2e）+ 前端 dev（代理指向该后端）：
 *
 *     BASE_URL=http://127.0.0.1:18097 bash scripts/verify/b2_dbsetup.sh keep
 *     cd frontend && BACKEND_ORIGIN=http://127.0.0.1:18097 pnpm dev
 *
 * 用法（从 frontend/ 目录跑，方便解析 node_modules）：
 *
 *     cd frontend && node ../scripts/verify/b2_config_page_check.mjs
 *
 * 断言只看 DOM（分组标题 / 开关类名 / 文案），截图只落 evidence。
 */

import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';

const WEB = process.env.B2_WEB_URL || 'http://127.0.0.1:5173';
const API = process.env.B2_API_URL || 'http://127.0.0.1:18097';
const OUT = '/opt/cloudpivot/evidence/20261102-erpb2';
const PASSWORDS = [process.env.B2_ADMIN_PASSWORD || 'Admin@123456', 'B2Admin#2026x'];

let PASS = 0;
let FAIL = 0;
const ok = (m) => { PASS += 1; console.log(`[PASS] ${m}`); };
const bad = (m) => { FAIL += 1; console.log(`[FAIL] ${m}`); };

async function login() {
  for (const password of PASSWORDS) {
    const res = await fetch(`${API}/api/system/auth/login`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username: 'admin', password }),
    });
    const body = await res.json();
    if (body.code === 0) return body.data;
  }
  throw new Error('admin 登录失败（试过的口令都不对）');
}

/** 库里四条开关的真实值（DOM 必须与它同源）。 */
async function dbSwitchState(token) {
  const res = await fetch(
    `${API}/api/support/config/page?page=1&page_size=200`,
    { headers: { Authorization: `Bearer ${token}` } },
  );
  const body = await res.json();
  const list = (body.data && body.data.list) || [];
  const wanted = list.filter((it) => (it.config_key || '').startsWith('erp.separation_of_duties.'));
  return wanted.map((it) => ({
    key: it.config_key,
    on: String(it.config_value).trim() === 'true',
    desc: it.description || '',
  }));
}

const auth = await login();
const expected = await dbSwitchState(auth.access_token);
if (expected.length !== 4) bad(`接口应返回 4 条内控参数，实际 ${expected.length}`);
else ok('接口返回 4 条内控参数');

const browser = await chromium.launch();
const page = await browser.newContext({ viewport: { width: 1440, height: 1200 } });
await page.addInitScript(
  ([a, r]) => {
    localStorage.setItem('x_access_token', a);
    localStorage.setItem('x_refresh_token', r);
    localStorage.setItem('x_guide_done', '1');
  },
  [auth.access_token, auth.refresh_token],
);
const tab = await page.newPage();
await tab.goto(`${WEB}/support/config`, { waitUntil: 'networkidle', timeout: 60000 });
await tab.waitForSelector('.config-group', { timeout: 30000 });

// ① 分组标题是「ERP 内控」（字典 config_category 的 item_label，不是裸编码）
const titles = await tab.$$eval('.config-group-title', (els) => els.map((e) => e.textContent.trim()));
if (titles.includes('ERP 内控')) ok(`参数页出现分组「ERP 内控」（共 ${titles.length} 组：${titles.join(' / ')}）`);
else bad(`参数页没有「ERP 内控」分组：${titles.join(' / ')}`);

// ② 该组下四条开关，且开/关状态与接口同源
const group = tab.locator('.config-group', { has: tab.locator('.config-group-title', { hasText: 'ERP 内控' }) });
const items = group.locator('.config-item');
const count = await items.count();
if (count === 4) ok('「ERP 内控」组内 4 条参数');
else bad(`「ERP 内控」组内应有 4 条参数，实际 ${count}`);

for (const want of expected) {
  const item = items.filter({ hasText: want.key });
  if ((await item.count()) === 0) {
    bad(`参数页没有 ${want.key}`);
    continue;
  }
  const switchEl = item.first().locator('.t-switch');
  const cls = (await switchEl.getAttribute('class')) || '';
  const isOn = cls.includes('t-is-checked');
  const label = (await switchEl.getAttribute('aria-checked')) || '';
  const realOn = isOn || label === 'true';
  if (realOn === want.on) ok(`${want.key} 页面显示${want.on ? '开启' : '关闭'}（与库一致）`);
  else bad(`${want.key} 页面显示 ${realOn ? '开启' : '关闭'}，库里是 ${want.on ? '开启' : '关闭'}（${cls}）`);
  if (want.desc && (await item.first().textContent()).includes(want.desc.slice(0, 8))) {
    ok(`${want.key} 展示了参数说明（管理员知道它管什么）`);
  }
}

// ③ 页面底部/分组里能读到「小团队可不开，内控要求上建议开启」这类说明
const body = await tab.textContent('body');
if (body.includes('小团队可不开') || body.includes('建议开启')) ok('页面能看到「小团队可不开 / 建议开启」的说明');
else bad('页面上没有内控强度说明（F6 说明文案缺失）');

await tab.screenshot({ path: `${OUT}/config-page-erp-internal-control.png`, fullPage: true });
console.log(`[info] 截图：${OUT}/config-page-erp-internal-control.png`);
await browser.close();

console.log(`\n== 结果 PASS=${PASS} FAIL=${FAIL}`);
process.exit(FAIL === 0 ? 0 : 1);
