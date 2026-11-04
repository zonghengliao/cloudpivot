#!/usr/bin/env node
/**
 * P2-7 账务巡检看板截图（Playwright / Chromium）
 *
 * 用法：
 *   BASE=http://127.0.0.1:5189 API=http://127.0.0.1:18095 \
 *   P27_USER=admin P27_PASS='P27Admin@123456_p27' \
 *   OUT=docs/evidence/20261105-p2-7-invariant-store/board-fail.png \
 *   node scripts/verify/p27_board_shot.mjs
 *
 * 做的事：
 *   1. 调登录接口拿令牌（不经过登录页，避免验证码 / 引导层干扰）；
 *   2. 把令牌写进 localStorage（前端 request.ts 读的键名 x_access_token / x_refresh_token）；
 *   3. 打开 /support/invariant，等「账务巡检看板」出现 + 表格/图表渲染完成；
 *   4. 整页截图，并把关键文案（指标 / 失败项 / 历史行数）打印成 SUMMARY 供证据核对。
 */
import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';
import fs from 'node:fs';
import path from 'node:path';

const BASE = process.env.BASE || 'http://127.0.0.1:5189';
const API = process.env.API || 'http://127.0.0.1:18095';
const USER = process.env.P27_USER || 'admin';
const PASS = process.env.P27_PASS || 'P27Admin@123456_p27';
const OUT = process.env.OUT || 'docs/evidence/20261105-p2-7-invariant-store/board-fail.png';

const loginRes = await fetch(`${API}/api/system/auth/login`, {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ username: USER, password: PASS }),
});
const loginBody = await loginRes.json().catch(() => ({}));
if (loginBody.code !== 0 || !loginBody.data?.access_token) {
  console.error('登录失败', loginRes.status, JSON.stringify(loginBody).slice(0, 300));
  process.exit(2);
}

const browser = await chromium.launch();
const context = await browser.newContext({ viewport: { width: 1600, height: 1200 }, locale: 'zh-CN' });
await context.addInitScript(
  ([a, r]) => {
    localStorage.setItem('x_access_token', a);
    localStorage.setItem('x_refresh_token', r);
    localStorage.setItem('x_guide_done', '1');
    localStorage.setItem('cp-onboarding-done', '1');
    localStorage.setItem('cp-guide-dismissed', '1');
  },
  [loginBody.data.access_token, loginBody.data.refresh_token],
);

const page = await context.newPage();
const consoleErrors = [];
page.on('console', (msg) => {
  if (msg.type() === 'error') consoleErrors.push(msg.text().slice(0, 200));
});
page.on('pageerror', (err) => consoleErrors.push('pageerror: ' + String(err).slice(0, 200)));

await page.goto(`${BASE}/support/invariant`, { waitUntil: 'domcontentloaded' });
await page.getByText('账务巡检看板').first().waitFor({ timeout: 30000 }).catch(() => {});
// 等首屏数据落地（表格出现 / 图表 canvas 出现）；等不到也照样截图 + 打印页面文本，便于定位
await page
  .waitForFunction(() => document.querySelectorAll('.t-table__body tbody tr').length > 0, null, { timeout: 20000 })
  .catch(() => {});
await page.waitForTimeout(1500);

const summary = await page.evaluate(() => {
  const text = (sel) => Array.from(document.querySelectorAll(sel)).map((el) => el.textContent?.trim() || '');
  return {
    title: document.querySelector('h3')?.textContent?.trim(),
    metrics: text('.invariant-metric'),
    failureRows: document.querySelectorAll('.invariant-page__card .t-table__body tbody tr').length,
    tables: document.querySelectorAll('table').length,
    canvases: document.querySelectorAll('canvas').length,
    statusTags: text('.t-tag'),
    bodyText: (document.body.innerText || '').slice(0, 1200),
  };
});

fs.mkdirSync(path.dirname(OUT), { recursive: true });
await page.screenshot({ path: OUT, fullPage: true });

console.log('SUMMARY', JSON.stringify(summary, null, 2));
console.log('CONSOLE_ERRORS', JSON.stringify(consoleErrors, null, 2));
console.log('SHOT', path.resolve(OUT));

await browser.close();
