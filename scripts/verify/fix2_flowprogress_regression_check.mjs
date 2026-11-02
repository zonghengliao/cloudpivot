#!/usr/bin/env node
/**
 * P1-FIX-2 回归：验证被改动的共享组件 `FlowProgress` 在**原有使用方**上没退化。
 *
 * ① 审批中心实例详情页（`/flow/instance/:id`）进度条仍在、节点顺序 START 第一；
 * ② 应付单页（P17 付款审批进度）打开无 console error；
 * ③ 收货单页（P16 作废审批进度）打开无 console error；
 * ④ 三页都无未捕获异常。
 */
import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';

const BASE = process.env.FIX2_BASE_URL || 'http://127.0.0.1:5197';
const INSTANCE_NO = process.env.FIX2_INSTANCE_ID || '';

let pass = 0;
let fail = 0;
const ok = (m) => { pass += 1; console.log(`[PASS] ${m}`); };
const bad = (m) => { fail += 1; console.log(`[FAIL] ${m}`); };

/** 会话：优先读 `FIX2_TOKEN_FILE`（第一行 access、第二行 refresh），避免反复登录撞限流 */
async function session() {
  const file = process.env.FIX2_TOKEN_FILE || '';
  if (file) {
    const { readFileSync } = await import('node:fs');
    const lines = readFileSync(file, 'utf8').trim().split('\n');
    if (lines[0]) {
      return { access_token: lines[0].trim(), refresh_token: (lines[1] || '').trim() };
    }
  }
  const res = await (await fetch(`${BASE}/api/system/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username: 'admin', password: 'Admin@123456' }),
  })).json();
  if (res.code !== 0) {
    console.error(`登录失败：${JSON.stringify(res).slice(0, 160)}`);
    process.exit(2);
  }
  return res.data;
}

const login = { data: await session() };
if (!login.data.access_token) {
  console.error('登录失败');
  process.exit(2);
}

// 拿一个真实实例 id（采购单 PO-20261031-000002 的审批单）
const page1 = await (await fetch(`${BASE}/api/business/erp/purchase-order/page?page=1&page_size=50`, {
  headers: { Authorization: `Bearer ${login.data.access_token}` },
})).json();
const po = page1.data.list.find((r) => r.order_no === 'PO-20261031-000002');
const instanceId = INSTANCE_NO || po?.approval_instance_id;

const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1600, height: 1100 } });
await ctx.addInitScript(([a, r]) => {
  localStorage.setItem('x_access_token', a);
  localStorage.setItem('x_refresh_token', r);
  localStorage.setItem('x_guide_done', '1');
  localStorage.setItem('cp-onboarding-done', '1');
}, [login.data.access_token, login.data.refresh_token]);
const page = await ctx.newPage();
const errors = [];
page.on('console', (m) => {
  if (m.type() === 'error' && !m.text().includes('Failed to load resource')) errors.push(m.text());
});
page.on('pageerror', (e) => errors.push(String(e)));

// ① 审批中心实例详情
if (!instanceId) {
  bad('① 拿不到实例 id');
} else {
  await page.goto(`${BASE}/flow/instance/${instanceId}`, { waitUntil: 'networkidle' });
  await page.waitForTimeout(800);
  const bar = page.locator('.flow-progress');
  if (await bar.count()) {
    const names = (await bar.first().locator('.flow-progress__name').allTextContents()).map((s) => s.trim());
    ok(`① 实例详情进度条仍在（${names.join(' → ')}）`);
    if (names[0] === '开始') ok('① 首节点是「开始」（排序修正在原使用方也生效）');
    else bad(`① 首节点不是「开始」：${names.join(' → ')}`);
    const taskBlocks = await page.locator('[data-testid="flow-progress-tasks"]').count();
    if (taskBlocks === 0) ok('① 实例详情没传 tasks → 不显示审批人明细（老页面渲染不变）');
    else bad('① 实例详情意外出现了审批人明细');
  } else {
    bad('① 实例详情进度条不见了（共享组件被改坏）');
  }
}

// ② 应付单页 / ③ 收货单页：打开无报错
for (const [label, path] of [
  ['② 应付单页', '/business/erp/payable'],
  ['③ 收货单页', '/business/erp/stock-in'],
]) {
  const before = errors.length;
  await page.goto(`${BASE}${path}`, { waitUntil: 'networkidle' });
  await page.waitForTimeout(600);
  const rows = await page.locator('tbody tr').count();
  if (rows > 0) ok(`${label} 列表正常渲染（${rows} 行）`);
  else bad(`${label} 列表为空（可能报错）`);
  if (errors.length === before) ok(`${label} 无 console error / 异常`);
  else bad(`${label} 报错：${errors.slice(before, before + 2).join(' | ')}`);
}

await browser.close();
console.log(`\n===== PASS ${pass} / FAIL ${fail} =====`);
process.exit(fail === 0 ? 0 : 1);
