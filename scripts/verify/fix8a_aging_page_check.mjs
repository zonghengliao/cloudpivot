#!/usr/bin/env node
/**
 * P1-FIX-8A 应付账龄分析：**真机页面验收**（DOM 断言 + 截图）。
 *
 * 验的是「财务在页面上能不能真的看懂这张表」，不是「接口能不能返 200」：
 * 表头档位、合计行逐格金额、供应商排序、明细逐行档位、改基准日后重算、菜单可见。
 * 每个数字都拿接口的**同一份响应**比对（DOM 必须与后端同源）。
 *
 * 前置：
 *   bash scripts/verify/fix8a_aging_db.sh recreate
 *   bash scripts/verify/fix8a_aging_instance.sh start
 *   bash scripts/verify/fix8a_aging_db.sh seed
 *   cd frontend && BACKEND_ORIGIN=http://127.0.0.1:18144 npx vite --host 127.0.0.1 --port 15144 --strictPort
 *
 * 用法（从 frontend/ 跑，方便解析 node_modules）：
 *   cd frontend && node ../scripts/verify/fix8a_aging_page_check.mjs
 */

import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';

const WEB = process.env.FIX8A_WEB || 'http://127.0.0.1:15144';
const API = process.env.FIX8A_API || 'http://127.0.0.1:18144';
const OUT = process.env.FIX8A_OUT || '/opt/cloudpivot/docs/evidence/20261105-p1fix8a-payable-aging';
// 隔离库首登会强制改密（fix8a_aging_e2e.py 已把它改成第二个），两个都试
const PASSWORDS = [process.env.FIX8A_PASS || 'Aging@2026Fix8', 'Admin@123456'];

let PASS = 0;
let FAIL = 0;
const ok = (m) => { PASS += 1; console.log(`[PASS] ${m}`); };
const bad = (m) => { FAIL += 1; console.log(`[FAIL] ${m}`); };
const eq = (label, actual, expected) => {
  if (String(actual).trim() === String(expected).trim()) ok(`${label} = ${String(actual).trim()}`);
  else bad(`${label}：页面「${String(actual).trim()}」≠ 后端「${expected}」`);
};

/** 金额比对：千分位文本与接口 numeric 都换算成「分」再比（不各写一套格式化口径） */
const centsOf = (text) => Math.round(Number(String(text ?? '0').replace(/,/g, '').trim()) * 100);
const eqAmount = (label, domText, apiValue) => {
  if (centsOf(domText) === Math.round(Number(String(apiValue)) * 100)) ok(`${label} = ${String(domText).trim()}`);
  else bad(`${label}：页面「${String(domText).trim()}」≠ 后端「${apiValue}」`);
};

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

async function apiReport(token, query) {
  const res = await fetch(`${API}/api/business/erp/report/payable-aging?${query}`, {
    headers: { Authorization: `Bearer ${token}`, x_access_token: token },
  });
  const body = await res.json();
  if (body.code !== 0) throw new Error(`报表接口失败：${JSON.stringify(body)}`);
  return body.data;
}

/** 逐块比对「当前页面上的表」与「接口同一基准日的表」 */
async function compareSheet(page, label, expected) {
  const headers = await page.$$eval("[data-testid='pa-table'] thead th", (els) =>
    els.map((e) => (e.textContent || '').trim()),
  );
  const wanted = [...expected.buckets.map((b) => b.label), '未付合计'];
  if (wanted.every((text) => headers.includes(text))) ok(`${label}：表头条数完整（${headers.join(' | ')}）`);
  else bad(`${label}：表头缺档位（${headers.join(' | ')}）`);

  const asOfText = (await page.textContent("[data-testid='pa-asof-text']")) || '';
  eq(`${label}：页头基准日`, asOfText.includes(expected.as_of), true);

  for (const cell of expected.summary) {
    eqAmount(
      `${label}：合计「${cell.label}」`,
      await page.textContent(`[data-testid='pa-sum-${cell.key}']`),
      cell.unpaid,
    );
  }
  eqAmount(`${label}：合计「未付合计」`, await page.textContent("[data-testid='pa-sum-total']"), expected.total_unpaid);
  eqAmount(`${label}：KPI 未付合计`, await page.textContent("[data-testid='pa-total-unpaid']"), expected.total_unpaid);
  eqAmount(`${label}：KPI 逾期合计`, await page.textContent("[data-testid='pa-overdue-unpaid']"), expected.overdue_unpaid);
  eq(`${label}：KPI 逾期占比`, await page.textContent("[data-testid='pa-overdue-ratio']"), `${expected.overdue_ratio}%`);
  eq(
    `${label}：KPI 供应商/单据`,
    await page.textContent("[data-testid='pa-counts']"),
    `${expected.supplier_count} 家 / ${expected.doc_count} 张`,
  );

  const supplierRows = await page.$$eval("[data-testid='pa-table'] tbody tr[data-testid^='pa-supplier-']", (els) => els.length);
  eq(`${label}：供应商行数`, supplierRows, expected.suppliers.length);
  for (let i = 0; i < expected.suppliers.length; i += 1) {
    eq(`${label}：供应商第 ${i + 1} 行`, await page.textContent(`[data-testid='pa-supplier-name-${i}']`), expected.suppliers[i].supplier_name);
  }

  const detailRows = await page.$$eval("[data-testid='pa-detail'] tbody tr", (els) => els.length);
  eq(`${label}：明细行数`, detailRows, expected.details.length);
  for (let i = 0; i < expected.details.length; i += 1) {
    const detail = expected.details[i];
    eq(`${label}：明细第 ${i + 1} 行 ${detail.payable_no} 档位`, await page.textContent(`[data-testid='pa-detail-bucket-${i}']`), detail.bucket_label);
    const daysText = detail.overdue_days > 0 ? `逾期 ${detail.overdue_days}` : detail.overdue_days === 0 ? '到期当天' : `未到期 ${-detail.overdue_days}`;
    eq(`${label}：明细第 ${i + 1} 行 ${detail.payable_no} 逾期天数文案`, await page.textContent(`[data-testid='pa-detail-days-${i}']`), daysText);
  }
}

const auth = await login();
const browser = await chromium.launch();
const context = await browser.newContext({ viewport: { width: 1560, height: 1100 } });
await context.addInitScript(
  ([a, r]) => {
    localStorage.setItem('x_access_token', a);
    localStorage.setItem('x_refresh_token', r);
    localStorage.setItem('x_guide_done', '1');
    localStorage.setItem('cp-onboarding-done', '1');
  },
  [auth.access_token, auth.refresh_token],
);
const page = await context.newPage();
// 用 domcontentloaded 而不是 networkidle：通知流（SSE）会一直保持连接，networkidle 永远等不到
await page.goto(`${WEB}/business/erp/report/payable-aging`, { waitUntil: 'domcontentloaded', timeout: 90000 });
await page.waitForSelector("[data-testid='pa-table']", { timeout: 60000 });

// ① 页头 / 菜单
const head = (await page.textContent("[data-testid='pa-head']")) || '';
if (head.includes('应付账款账龄分析表')) ok('页头标题 = 应付账款账龄分析表');
else bad(`页头标题不对：${head.slice(0, 80)}`);
const bodyText = (await page.textContent('body')) || '';
if (bodyText.includes('应付账龄分析')) ok('侧边栏能看到「应付账龄分析」（V0255 菜单 + 路由都在）');
else bad('页面上找不到「应付账龄分析」菜单项');

// ② 选期间 2026-11 → 基准日自动 = 期末日 2026-11-30（期间 → 期末日的接线）
await page.click("[data-testid='pa-period-select']");
await page.click(".t-select-option:has-text('2026-11')");
await page.waitForFunction(
  () => (document.querySelector("[data-testid='pa-asof-text']")?.textContent || '').includes('2026-11-30'),
  { timeout: 20000 },
).catch(() => {});
const byPeriod = await apiReport(auth.access_token, 'period=2026-11');
await compareSheet(page, '按期间(2026-11-30)', byPeriod);
const asOfHead = (await page.textContent("[data-testid='pa-asof-text']")) || '';
if (asOfHead.includes('期间末日')) ok('基准日来源标为「期间末日」');
else bad(`基准日来源文案不对：${asOfHead}`);

// ③ 手填基准日 2026-11-05 → 五个档位全部落在边界上
await page.fill("[data-testid='pa-asof'] input", '2026-11-05');
await page.keyboard.press('Enter');
await page.click("[data-testid='pa-search']");
await page.waitForFunction(
  () => (document.querySelector("[data-testid='pa-asof-text']")?.textContent || '').includes('2026-11-05'),
  { timeout: 20000 },
).catch(() => {});
const byDay = await apiReport(auth.access_token, 'period=2026-11&as_of=2026-11-05');
await compareSheet(page, '手填基准日(2026-11-05)', byDay);
const asOfHead2 = (await page.textContent("[data-testid='pa-asof-text']")) || '';
if (asOfHead2.includes('手动指定')) ok('基准日来源标为「手动指定」');
else bad(`手填基准日后来源文案不对：${asOfHead2}`);
await page.screenshot({ path: `${OUT}/01-payable-aging-2026-11-05.png`, fullPage: true });
console.log(`[info] 截图：${OUT}/01-payable-aging-2026-11-05.png`);

// ④ 基准日 +1 天 → 「到期当天」那张挪进 1-30；30 天那张挪到 31-60
await page.fill("[data-testid='pa-asof'] input", '2026-11-06');
await page.keyboard.press('Enter');
await page.click("[data-testid='pa-search']");
await page.waitForFunction(
  () => (document.querySelector("[data-testid='pa-asof-text']")?.textContent || '').includes('2026-11-06'),
  { timeout: 20000 },
).catch(() => {});
const nextDay = await apiReport(auth.access_token, 'period=2026-11&as_of=2026-11-06');
eqAmount('2026-11-06：合计「未到期」', await page.textContent("[data-testid='pa-sum-NOT_DUE']"), nextDay.summary.find((c) => c.key === 'NOT_DUE').unpaid);
eqAmount('2026-11-06：合计「1-30 天」', await page.textContent("[data-testid='pa-sum-D0_30']"), nextDay.summary.find((c) => c.key === 'D0_30').unpaid);
const movedIndex = nextDay.details.findIndex((d) => d.payable_no === 'AP-FIX8A-000002');
eq('2026-11-06：到期当天那张的档位', await page.textContent(`[data-testid='pa-detail-bucket-${movedIndex}']`), '1-30 天');
await page.screenshot({ path: `${OUT}/02-payable-aging-2026-11-06.png`, fullPage: true });
console.log(`[info] 截图：${OUT}/02-payable-aging-2026-11-06.png`);

await browser.close();
console.log(`\n== 结果 PASS=${PASS} FAIL=${FAIL}`);
process.exit(FAIL === 0 ? 0 : 1);
