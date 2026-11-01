#!/usr/bin/env node
/**
 * D5 页面验收：应付单详情抽屉「发起付款」（Playwright，打**隔离实例**的前端入口）。
 *
 * ```bash
 * BACKEND_ORIGIN=http://127.0.0.1:18095 npx vite --port 5195 &   # frontend 目录
 * D5_BASE_URL=http://127.0.0.1:5195 node ../scripts/verify/d5_page_check.mjs
 * ```
 *
 * 断言只看 DOM（`data-testid` / 文案 / `input.value`）：
 *
 * 1. `AP-D5PG-OPEN`（未付 226）→ 详情抽屉出现「发起付款（未付 226.00）」按钮；
 * 2. 点它 → 付款弹窗的**金额输入框预填 226**（F2：未付，不是应付总额；此处两者相等，
 *    所以真正的 F2 断言在下面部分付款那张上）；
 * 3. `AP-D5PG-PARTIAL`（应付 226 / 已付 100）→ 按钮文案是「未付 126.00」，点开预填 **126**；
 * 4. `AP-D5PG-SETTLED` → **没有**「发起付款」按钮，改成一句「已结清」说明（L9）；
 * 5. 全程没有 console error / 未捕获异常。
 *
 * 只读：全程不点「确认付款」，不会在库里留下任何付款记录。
 */
import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';

const BASE = process.env.D5_BASE_URL || 'http://127.0.0.1:5195';
const USER = process.env.D5_ADMIN_USER || 'admin';
const PASSWORD = process.env.D5_ADMIN_PASSWORD || process.env.D5_ADMIN_NEW_PASSWORD || 'D5Admin#2026x';
const PASSWORD_FALLBACK = 'Admin@123456';

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

async function apiLogin(password) {
  const res = await fetch(`${BASE}/api/system/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username: USER, password }),
  });
  const body = await res.json().catch(() => ({}));
  if (body.code !== 0) return null;
  return body.data;
}

(async () => {
  let session = await apiLogin(PASSWORD);
  if (!session) session = await apiLogin(PASSWORD_FALLBACK);
  if (!session) {
    console.error('登录失败：口令既不是 D5_ADMIN_PASSWORD 也不是 Admin@123456');
    process.exit(2);
  }
  const { access_token: accessToken, refresh_token: refreshToken } = session;

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
  const errors = [];
  /** 4xx/5xx 的接口（console 里的 `Failed to load resource` 看不出是哪一个，这里记 URL） */
  const httpErrors = [];
  page.on('console', (msg) => {
    if (msg.type() === 'error') errors.push(msg.text());
  });
  page.on('response', (resp) => {
    if (resp.status() >= 400) httpErrors.push(`${resp.status()} ${resp.url()}`);
  });
  page.on('pageerror', (error) => errors.push(String(error)));

  /** 用深链打开某张应付单的详情抽屉 */
  async function openPayable(no) {
    await page.goto(`${BASE}/business/erp/payable?payable_no=${no}`, { waitUntil: 'networkidle' });
    await page.waitForSelector('[data-testid="payable-detail"]', { timeout: 20000 });
    // 等可付信息回来（按钮/说明二选一出现）
    await page.waitForSelector(
      '[data-testid="btn-payable-drawer-pay"], [data-testid="payable-pay-blocked"]',
      { timeout: 20000 },
    );
  }

  // ---- ① OPEN：按钮显示，文案带未付 ----
  await openPayable('AP-D5PG-OPEN');
  const payBtn = page.locator('[data-testid="btn-payable-drawer-pay"]');
  if (await payBtn.isVisible()) {
    ok('① OPEN 应付：详情抽屉出现「发起付款」按钮');
  } else {
    bad('① OPEN 应付：没有「发起付款」按钮');
  }
  const openText = (await payBtn.textContent()) || '';
  if (openText.includes('226')) {
    ok(`① 按钮文案带出未付金额（${openText.trim()}）`);
  } else {
    bad(`① 按钮文案没有未付金额：${openText.trim()}`);
  }

  await payBtn.click();
  await page.waitForSelector('[data-testid="payable-pay-dialog"]', { timeout: 10000 });
  const openAmount = await page.locator('[data-testid="payable-pay-amount"] input').inputValue();
  if (openAmount.replace(/,/g, '') === '226' || openAmount.replace(/,/g, '') === '226.00') {
    ok(`① 付款弹窗金额预填未付（${openAmount}）`);
  } else {
    bad(`① 付款弹窗金额预填不是未付：${openAmount}`);
  }
  // 关掉弹窗（不提交）：卡面要求全程不产生付款记录
  await page.keyboard.press('Escape');
  await page.waitForTimeout(300);

  // ---- ② PARTIAL：按钮文案 126、预填 126（F2 真断言：不是 226） ----
  await openPayable('AP-D5PG-PARTIAL');
  const partialBtn = page.locator('[data-testid="btn-payable-drawer-pay"]');
  const partialText = (await partialBtn.textContent()) || '';
  if (partialText.includes('126') && !partialText.includes('226')) {
    ok(`② 部分已付：按钮带的是**未付 126**（${partialText.trim()}）`);
  } else {
    bad(`② 部分已付：按钮文案不对（${partialText.trim()}）`);
  }
  await partialBtn.click();
  await page.waitForSelector('[data-testid="payable-pay-dialog"]', { timeout: 10000 });
  const partialAmount = await page.locator('[data-testid="payable-pay-amount"] input').inputValue();
  if (['126', '126.00'].includes(partialAmount.replace(/,/g, ''))) {
    ok(`② 预填金额 = 未付 126（不是应付总额 226）`);
  } else {
    bad(`② 预填金额不是 126：${partialAmount}`);
  }
  await page.keyboard.press('Escape');
  await page.waitForTimeout(300);

  // ---- ③ SETTLED：没有按钮，只有「已结清」说明 ----
  await openPayable('AP-D5PG-SETTLED');
  const settledBtn = page.locator('[data-testid="btn-payable-drawer-pay"]');
  if ((await settledBtn.count()) === 0) {
    ok('③ 已结清：没有「发起付款」按钮（L9）');
  } else {
    bad('③ 已结清：按钮不该出现');
  }
  const blocked = page.locator('[data-testid="payable-pay-blocked"]');
  const blockedText = (await blocked.textContent().catch(() => '')) || '';
  if (blockedText.includes('已结清')) {
    ok(`③ 已结清：给出一句原因（${blockedText.trim()}）`);
  } else {
    bad(`③ 已结清：没有说明原因（${blockedText.trim()}）`);
  }

  // ---- ④ 全程零 console error ----
  // ⚠️ `/api/business/notice/stream`（站内通知 SSE）在所有页面都会连一次，隔离实例上它恒 401
  //    （SSE 通道不在本卡的验收范围，且与应付页无关：P7/P17 的页面验收同样带这条噪声）。
  //    其余任何 console error / HTTP ≥ 400 都必须为 0。
  const IGNORED = /notice\/stream/;
  // `Failed to load resource` 是上面那些 4xx 的浏览器回声（已按 URL 过滤），其余才是真异常
  const fatalConsole = errors.filter((text) => !text.startsWith('Failed to load resource'));
  const fatalHttp = httpErrors.filter((text) => !IGNORED.test(text));
  if (fatalHttp.length === 0 && fatalConsole.length === 0) {
    ok(`④ 无 console error（除通知 SSE 的 ${httpErrors.length} 次 401，与应付页无关）`);
  } else {
    bad(
      `④ console error：${fatalConsole.slice(0, 3).join(' | ')}；HTTP ≥400：${fatalHttp.slice(0, 5).join(' | ')}`,
    );
  }

  await browser.close();
  console.log(`\n== 汇总：${PASS} PASS / ${FAIL} FAIL ==`);
  process.exit(FAIL === 0 ? 0 : 1);
})();
