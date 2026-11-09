#!/usr/bin/env node
/**
 * D5 生产页面验收（Playwright，打 **144 的 nginx → 现网 dist → 8023**）。
 *
 * 数据**全部取自生产真实应付单**（不造夹具、不写库）：先从接口拿两张应付单
 * （一张可付、一张不可付），把接口给出的期望值当作断言基线，再去 DOM 上核对 ——
 * 这样「页面显示的数字」和「后端算的数字」是同一份真值，不存在两套口径。
 *
 * 断言：
 * 1. 可付的那张：详情抽屉出现「发起付款（未付 X）」且 X = 接口的 `default_amount`；
 * 2. 点按钮 → 付款弹窗金额预填 = `default_amount`（**未付**，不是应付总额）；**不提交**；
 * 3. 不可付的那张（已作废 / 已结清）：**没有**「发起付款」按钮，改成接口给的 `blocked_reason`；
 * 4. 无 console error（站内通知 SSE 的 401 除外，与应付页无关）。
 *
 * ```bash
 * # 144 上（口令从 /etc/cloudpivot/cloudpivot.env 来，别写进仓库）
 * set -a; . /etc/cloudpivot/cloudpivot.env; set +a
 * D5_PROD_BASE_URL=http://127.0.0.1 node scripts/verify/d5_prod_page_check.mjs
 * ```
 */
import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';

const BASE = process.env.D5_PROD_BASE_URL || 'http://127.0.0.1';
// 登录口径（卡 t_d4e92b99）：运维账号 CLOUDPIVOT_OPS_*，**单次尝试**。
// 原来的「候选口令列表 + 逐个重试」是锁号放大器：每错一次 login_fail_count+1，5 次锁 30 分钟。
const USER = process.env.D5_PROD_USER || process.env.CLOUDPIVOT_OPS_USER || 'admin';
const PASSWORD =
  process.env.D5_PROD_PASSWORD ||
  process.env.CLOUDPIVOT_OPS_PASSWORD ||
  process.env.NIK_ADMIN_PASSWORD ||
  '';

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

async function login() {
  if (!PASSWORD) return null;
  const res = await fetch(`${BASE}/api/system/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username: USER, password: PASSWORD }),
  });
  const body = await res.json().catch(() => ({}));
  if (body.code === 0 && body.data?.access_token) return body.data;
  console.error(`  登录 HTTP ${res.status}（账号 ${USER}；只试一次，401/403 不要重试）`);
  return null;
}

const api = async (token, path) => {
  const res = await fetch(`${BASE}${path}`, { headers: { Authorization: `Bearer ${token}` } });
  const body = await res.json().catch(() => ({}));
  return { status: res.status, body };
};

/** 金额按数值比（`226.0000` 与 `226.00` 是同一个数） */
const sameMoney = (a, b) => Math.abs(Number(a) - Number(b)) < 1e-6;
const money = (text) => Number(String(text).replace(/[^\d.-]/g, ''));

(async () => {
  const session = await login();
  if (!session) {
    console.error('登录失败：核对 CLOUDPIVOT_OPS_USER / CLOUDPIVOT_OPS_PASSWORD（只试一次，不重试）');
    process.exit(2);
  }
  const { access_token: accessToken, refresh_token: refreshToken } = session;

  const list = await api(accessToken, '/api/business/erp/payable/page?page=1&page_size=50');
  const rows = list.body?.data?.list ?? [];
  if (rows.length === 0) {
    console.error('生产没有应付单，无法做页面验收');
    process.exit(2);
  }

  const withInfo = [];
  for (const row of rows) {
    const info = await api(accessToken, `/api/business/erp/payable/${row.id}/payment-info`);
    const pay = info.body?.data?.payable;
    if (pay) withInfo.push(pay);
  }
  const payable = withInfo.find((item) => item.can_pay);
  const blocked = withInfo.find((item) => !item.can_pay);
  console.log(
    `样本：可付 ${payable?.payable_no ?? '无'}（未付 ${payable?.default_amount ?? '-'}） / ` +
      `不可付 ${blocked?.payable_no ?? '无'}（${blocked?.blocked_reason ?? '-'}）`,
  );

  const browser = await chromium.launch();
  const context = await browser.newContext({ viewport: { width: 1440, height: 1200 } });
  await context.addInitScript(
    ([a, r]) => {
      localStorage.setItem('x_access_token', a);
      localStorage.setItem('x_refresh_token', r);
      localStorage.setItem('x_guide_done', '1');
      localStorage.setItem('cp-onboarding-done', '1');
    },
    [accessToken, refreshToken],
  );
  const page = await context.newPage();
  const consoleErrors = [];
  const httpErrors = [];
  page.on('console', (msg) => {
    if (msg.type() === 'error') consoleErrors.push(msg.text());
  });
  page.on('response', (resp) => {
    if (resp.status() >= 400) httpErrors.push(`${resp.status()} ${resp.url()}`);
  });
  page.on('pageerror', (err) => consoleErrors.push(String(err)));

  async function openPayable(no) {
    await page.goto(`${BASE}/business/erp/payable?payable_no=${no}`, { waitUntil: 'networkidle' });
    await page.waitForSelector('[data-testid="payable-detail"]', { timeout: 25000 });
    await page.waitForSelector(
      '[data-testid="btn-payable-drawer-pay"], [data-testid="payable-pay-blocked"]',
      { timeout: 25000 },
    );
  }

  // ---- ① 可付的那张 ----
  if (!payable) {
    bad('① 生产没有「可付」应付单样本（本次只验不可付侧）');
  } else {
    await openPayable(payable.payable_no);
    const btn = page.locator('[data-testid="btn-payable-drawer-pay"]');
    if (await btn.isVisible()) {
      ok(`① ${payable.payable_no} 详情出现「发起付款」按钮`);
    } else {
      bad(`① ${payable.payable_no} 没有「发起付款」按钮`);
    }
    const text = (await btn.textContent()) || '';
    if (text.includes(String(money(payable.default_amount)))) {
      ok(`① 按钮文案带出未付金额（${text.trim()}）`);
    } else {
      bad(`① 按钮文案与接口 default_amount=${payable.default_amount} 不一致：${text.trim()}`);
    }
    await btn.click();
    await page.waitForSelector('[data-testid="payable-pay-dialog"]', { timeout: 15000 });
    const amount = await page.locator('[data-testid="payable-pay-amount"] input').inputValue();
    if (sameMoney(money(amount), payable.default_amount)) {
      ok(`① 付款弹窗预填 = 未付 ${payable.default_amount}（不是应付总额 ${payable.amount}）`);
    } else {
      bad(`① 付款弹窗预填 ${amount} ≠ 接口 default_amount ${payable.default_amount}`);
    }
    await page.keyboard.press('Escape'); // 不提交：生产不产生付款
    await page.waitForTimeout(400);
  }

  // ---- ② 不可付的那张 ----
  if (!blocked) {
    bad('② 生产没有「不可付」应付单样本（本次只验可付侧）');
  } else {
    await openPayable(blocked.payable_no);
    const count = await page.locator('[data-testid="btn-payable-drawer-pay"]').count();
    if (count === 0) {
      ok(`② ${blocked.payable_no}（${blocked.status}）没有「发起付款」按钮`);
    } else {
      bad(`② ${blocked.payable_no}（${blocked.status}）不该出现按钮`);
    }
    const blockedText = (await page
      .locator('[data-testid="payable-pay-blocked"]')
      .textContent()
      .catch(() => '')) || '';
    const keyword = String(blocked.blocked_reason).slice(0, 12);
    if (blockedText.includes(keyword)) {
      ok(`② 页面原因与接口一致（${blockedText.trim().slice(0, 60)}…）`);
    } else {
      bad(`② 页面原因「${blockedText.trim()}」与接口「${blocked.blocked_reason}」不一致`);
    }
  }

  // ---- ③ 噪声控制 ----
  const IGNORED = /notice\/stream/;
  const fatalHttp = httpErrors.filter((text) => !IGNORED.test(text));
  const fatalConsole = consoleErrors.filter((text) => !text.startsWith('Failed to load resource'));
  if (fatalHttp.length === 0 && fatalConsole.length === 0) {
    ok(`③ 无 console error（通知 SSE 的 ${httpErrors.length} 次 401 已排除）`);
  } else {
    bad(`③ console error：${fatalConsole.slice(0, 3).join(' | ')}；HTTP ≥400：${fatalHttp.slice(0, 5).join(' | ')}`);
  }

  await browser.close();
  console.log(`\n== 汇总：${PASS} PASS / ${FAIL} FAIL ==`);
  process.exit(FAIL === 0 ? 0 : 1);
})();
