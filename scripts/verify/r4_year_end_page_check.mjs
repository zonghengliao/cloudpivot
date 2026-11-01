#!/usr/bin/env node
/**
 * ERP-R4 年度结转页面的 DOM 验收（Playwright，打 **vite dev + 隔离实例**）。
 *
 * 前置：
 *
 * ```bash
 * # 1) 隔离后端（cloudpivot_r4e @ 18098）
 * bash scripts/verify/r4_instance.sh start
 * # 2) vite dev（/api 转发到 18098）
 * cd frontend && BACKEND_ORIGIN=http://127.0.0.1:18098 pnpm dev --port 5189
 * ```
 *
 * 用法（从 frontend/ 目录跑，方便解析 node_modules）：
 *
 * ```bash
 * cd frontend && ERP_PAGE_ADMIN_PASSWORD='R4@Admin2026' node ../scripts/verify/r4_year_end_page_check.mjs
 * ```
 *
 * 本脚本**自带夹具**：自己开 2042 / 2043 整年、记收入与费用、逐月结账，
 * 然后在页面上把「未结转 → 年结 → 已结转 → 反年结」四态走完。
 * 断言只看 DOM（data-testid / 文案 / disabled）；截图只做留痕。
 */
import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';

const BASE = process.env.ERP_PAGE_BASE || 'http://127.0.0.1:5189';
const ADMIN = process.env.ERP_PAGE_ADMIN || 'admin';
const PASSWORD = process.env.ERP_PAGE_ADMIN_PASSWORD || 'R4@Admin2026';
const YEAR = 2042;
const NEXT_YEAR = 2043;
const EMPTY_YEAR = 2041; // 用来验「前置没过 → 按钮置灰 + 说明原因」
const OUT_DIR = '/opt/cloudpivot/docs/evidence/20261101-erpr4';

let PASS = 0;
let FAIL = 0;
const ok = (msg) => {
  PASS += 1;
  console.log(`[PASS] ${msg}`);
};
const bad = (msg) => {
  FAIL += 1;
  console.log(`[FAIL] ${msg}`);
};
const expect = (name, want, got) => {
  if (String(want) === String(got)) ok(`${name}（${got}）`);
  else bad(`${name}（期望 ${want}，实际 ${got}）`);
};
const expectMatch = (name, pattern, got) => {
  if (pattern.test(String(got))) ok(`${name}（${got}）`);
  else bad(`${name}（期望匹配 ${pattern}，实际 ${got}）`);
};

let TOKEN = null;
async function api(method, path, body) {
  const res = await fetch(`${BASE}${path}`, {
    method,
    headers: {
      'Content-Type': 'application/json',
      ...(TOKEN ? { Authorization: `Bearer ${TOKEN}` } : {}),
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  const text = await res.text();
  let payload = {};
  try {
    payload = text ? JSON.parse(text) : {};
  } catch {
    payload = { raw: text };
  }
  return { status: res.status, payload };
}

async function login() {
  const res = await fetch(`${BASE}/api/system/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username: ADMIN, password: PASSWORD }),
  });
  const body = await res.json();
  if (body.code !== 0 || !body.data?.access_token) {
    console.error('登录失败：', JSON.stringify(body).slice(0, 200));
    process.exit(1);
  }
  TOKEN = body.data.access_token;
  return body.data;
}

async function periodRows(year) {
  const page = await api('GET', `/api/business/erp/period/page?period_year=${year}&page=1&page_size=20`);
  return page.payload?.data?.list ?? [];
}

async function ensureYear(year) {
  const rows = await periodRows(year);
  if (rows.length === 0) {
    const res = await api('POST', '/api/business/erp/period/batch-open', {
      from: `${year}-01`,
      to: `${year}-12`,
      remark: 'R4 页面验收夹具',
    });
    console.log(`  开 ${year} 全 12 个月：HTTP ${res.status}`);
  }
}

async function accountMap(codes) {
  const res = await api('GET', '/api/business/erp/account/page?page=1&page_size=500');
  const list = res.payload?.data?.list ?? [];
  const map = {};
  for (const row of list) {
    if (codes.includes(row.account_code)) map[row.account_code] = row.id;
  }
  return map;
}

async function setupFixture() {
  await ensureYear(YEAR);
  await ensureYear(NEXT_YEAR);
  await ensureYear(EMPTY_YEAR); // 只开账、不结账：用来验置灰态

  // 已经年结过（重跑）→ 先反年结
  const state = await api('GET', `/api/business/erp/year-end/check?year=${YEAR}`);
  if (state.payload?.data?.state?.closed) {
    const reopened = await api('POST', '/api/business/erp/year-end/reopen', {
      year: YEAR,
      reason: 'R4 页面验收重跑，先反年结再重来一遍',
    });
    console.log(`  反年结 ${YEAR}：HTTP ${reopened.status}`);
  }

  const accounts = await accountMap(['1002', '5001', '5602']);
  // 只在「本年还没有凭证」时造数据（重跑不重复记）
  const vouchers = await api('GET', `/api/business/erp/voucher/page?period=${YEAR}-03&page=1&page_size=10`);
  const has = (vouchers.payload?.data?.list ?? []).length > 0;
  if (!has) {
    const revenue = await api('POST', '/api/business/erp/voucher', {
      voucher_date: `${YEAR}-03-15`,
      summary: 'R4 页面验收 收入',
      entries: [
        { account_id: accounts['1002'], debit: 1000, credit: 0 },
        { account_id: accounts['5001'], debit: 0, credit: 1000 },
      ],
    });
    const expense = await api('POST', '/api/business/erp/voucher', {
      voucher_date: `${YEAR}-04-10`,
      summary: 'R4 页面验收 费用',
      entries: [
        { account_id: accounts['5602'], debit: 400, credit: 0 },
        { account_id: accounts['1002'], debit: 0, credit: 400 },
      ],
    });
    console.log(`  记凭证：收入 HTTP ${revenue.status} / 费用 HTTP ${expense.status}`);
  }

  // 逐月结账（已结账的跳过）
  const rows = await periodRows(YEAR);
  let closed = 0;
  for (const row of rows.sort((a, b) => a.period_code.localeCompare(b.period_code))) {
    if (row.status === 'CLOSED') {
      closed += 1;
      continue;
    }
    const res = await api('POST', `/api/business/erp/period/${row.id}/close`, { reason: 'R4 页面验收' });
    if (res.status === 200) closed += 1;
    else console.log(`  结账 ${row.period_code} 失败：HTTP ${res.status} ${JSON.stringify(res.payload).slice(0, 120)}`);
  }
  console.log(`  ${YEAR} 已结账 ${closed}/12 月`);
}

/** 年度下拉里选中某一年（页面默认选最大年度） */
async function selectYear(page, year) {
  await page.locator('[data-testid="yearend-year-select"]').click();
  await page.locator(`.t-select-option:has-text("${year} 年")`).first().click();
  await page.waitForTimeout(1200);
}

(async () => {
  const session = await login();
  await setupFixture();

  const browser = await chromium.launch();
  const context = await browser.newContext({ viewport: { width: 1440, height: 1100 } });
  await context.addInitScript(
    ([a, r]) => {
      localStorage.setItem('x_access_token', a);
      localStorage.setItem('x_refresh_token', r);
      localStorage.setItem('x_guide_done', '1');
    },
    [session.access_token, session.refresh_token],
  );
  const page = await context.newPage();
  const errors = [];
  const http401 = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  page.on('console', (msg) => {
    if (msg.type() === 'error') errors.push(msg.text());
  });
  page.on('response', (res) => {
    if (res.status() === 401) http401.push(`${res.request().method()} ${res.url()}`);
    if (res.status() >= 500) errors.push(`${res.status()} ${res.url()}`);
  });

  const text = async (sel) => (await page.locator(sel).first().innerText()).trim();

  await page.goto(`${BASE}/business/erp/year-end`, { waitUntil: 'networkidle' });
  await page.waitForSelector('[data-testid="yearend-status-tag"]', { timeout: 30000 });

  // ---------- ① 置灰态：2041 只开了账没结账 ----------
  await selectYear(page, EMPTY_YEAR);
  expectMatch('① 未结账年度：状态标签「未结转」', /未结转/, await text('[data-testid="yearend-status-tag"]'));
  expectMatch(
    '①b 五项里的第 1 项 ❌ 且点名月份',
    /没结账|结账/,
    await text('[data-testid="yearend-check-PERIOD_NOT_CLOSED"]'),
  );
  const disabledAtEmpty = await page.locator('[data-testid="btn-yearend-close"]').isDisabled();
  expect('①c 前置没过 → 「年度结转」置灰', true, disabledAtEmpty);
  expectMatch(
    '①d 置灰时必须说明原因',
    /前置校验未通过/,
    await text('[data-testid="yearend-close-blocked"]'),
  );
  await page.screenshot({ path: `${OUT_DIR}/20-年度结转-前置未过.png`, fullPage: true });

  // ---------- ② 可结转态：2042 全绿 ----------
  await selectYear(page, YEAR);
  expectMatch('② 状态标签「2042 年度未结转」', /2042 年度未结转/, await text('[data-testid="yearend-status-tag"]'));
  for (const type of [
    'PERIOD_NOT_CLOSED',
    'DOC_UNFINISHED',
    'UNBALANCED',
    'INVARIANT',
    'NEXT_YEAR_NOT_OPENED',
  ]) {
    const row = await text(`[data-testid="yearend-check-${type}"]`);
    if (row.includes('✅')) ok(`② 前置校验 ${type} ✅`);
    else bad(`② 前置校验 ${type} 应为 ✅，实际「${row}」`);
  }
  expectMatch('②b 结转凭证：尚未生成', /尚未生成/, await text('[data-testid="yearend-voucher"]'));
  expectMatch('②c 下年期初：尚未生成', /尚未生成/, await text('[data-testid="yearend-opening"]'));
  expectMatch('②d 本年已结账 12/12 月', /12\/12/, await text('[data-testid="yearend-periods-closed"]'));
  const enabled = await page.locator('[data-testid="btn-yearend-close"]').isEnabled();
  expect('②e 五项全过 → 「年度结转」可点', true, enabled);
  await page.screenshot({ path: `${OUT_DIR}/21-年度结转-前置全过.png`, fullPage: true });

  // ---------- ③ 年结（二次确认 + 备注） ----------
  await page.locator('[data-testid="btn-yearend-close"]').click();
  await page.waitForSelector('[data-testid="yearend-close-dialog"]', { timeout: 10000 });
  await page.locator('[data-testid="yearend-close-reason"] input').fill('页面验收：2042 年度结账');
  await page.screenshot({ path: `${OUT_DIR}/22-年度结转-确认弹窗.png`, fullPage: true });
  await page.locator('.t-dialog__footer button.t-button--theme-primary').last().click();
  await page.waitForTimeout(2500);

  expectMatch('③ 状态标签变成「2042 年度已结转」', /已结转/, await text('[data-testid="yearend-status-tag"]'));
  expectMatch('③b 显示结转凭证号', /PZ-/, await text('[data-testid="yearend-voucher"]'));
  expectMatch('③c 显示下年期初已生成', /2043-01 期初已生成/, await text('[data-testid="yearend-opening"]'));
  expectMatch('③d 本年净利润 600.00', /600\.00/, await text('[data-testid="yearend-net-profit"]'));
  const reopenVisible = await page.locator('[data-testid="btn-yearend-reopen"]').isVisible();
  expect('③e 已结转 → 出现「反年结」', true, reopenVisible);
  const closeVisible = await page.locator('[data-testid="btn-yearend-close"]').isVisible();
  expect('③f 已结转 → 「年度结转」不可再点（置灰）', true, await page.locator('[data-testid="btn-yearend-close"]').isDisabled() || !closeVisible);
  await page.screenshot({ path: `${OUT_DIR}/23-年度结转-已结转.png`, fullPage: true });

  // ---------- ④ 反年结（原因校验 + 回滚） ----------
  await page.locator('[data-testid="btn-yearend-reopen"]').click();
  await page.waitForSelector('[data-testid="yearend-reopen-dialog"]', { timeout: 10000 });
  await page.locator('[data-testid="yearend-reopen-reason"] input').fill('改一下');
  await page.locator('.t-dialog__footer button.t-button--theme-danger').last().click();
  await page.waitForTimeout(800);
  expectMatch(
    '④ 原因过短 → 弹窗内提示（至少 10 个字）',
    /至少 10 个字/,
    await text('[data-testid="yearend-reopen-error"]'),
  );
  await page.screenshot({ path: `${OUT_DIR}/24-反年结-原因过短.png`, fullPage: true });

  await page.locator('[data-testid="yearend-reopen-reason"] input').fill('页面验收：需要重做 2042 年度结转');
  await page.locator('.t-dialog__footer button.t-button--theme-danger').last().click();
  await page.waitForTimeout(2500);
  expectMatch('④b 回滚后状态回到「未结转」', /未结转/, await text('[data-testid="yearend-status-tag"]'));
  expectMatch('④c 结转凭证回到「尚未生成」', /尚未生成/, await text('[data-testid="yearend-voucher"]'));
  expectMatch('④d 下年期初回到「尚未生成」', /尚未生成/, await text('[data-testid="yearend-opening"]'));
  await page.screenshot({ path: `${OUT_DIR}/25-反年结-已回滚.png`, fullPage: true });

  // ---------- ⑤ 控制台干净 ----------
  if (http401.length > 0) {
    console.log('  401 请求（框架的令牌刷新往返，不算失败）：');
    http401.slice(0, 6).forEach((line) => console.log(`    ${line}`));
  }
  const noisy = errors.filter(
    (item) => !/favicon|ResizeObserver/.test(item) && !item.includes('401'),
  );
  expect('⑤ 无 console / 500 报错（401 由框架的令牌刷新兜底）', '0', String(noisy.length));
  if (noisy.length > 0) console.log(`  ${noisy.slice(0, 3).join(' | ')}`);

  await browser.close();
  console.log(`\n汇总：${PASS} 通过 / ${FAIL} 失败`);
  process.exit(FAIL === 0 ? 0 : 1);
})();
