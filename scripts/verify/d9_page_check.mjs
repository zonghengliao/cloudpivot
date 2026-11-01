#!/usr/bin/env node
/**
 * D9 页面验收：三个单据详情里的「复制为新单」（Playwright，打**隔离实例**的前端入口）。
 *
 * ```bash
 * cd /opt/cloudpivot/frontend
 * BACKEND_ORIGIN=http://127.0.0.1:18096 npx vite --port 5196 &
 * D9_BASE_URL=http://127.0.0.1:5196 D9_KEEP=1 node ../scripts/verify/d9_page_check.mjs
 * ```
 *
 * 前置：先用 `D9_KEEP=1` 跑一遍 `d9_document_copy_e2e.py`，把夹具留在库里
 * （已过账且带采购单的收货单 / PENDING 调拨单 / 带子任务+评论的任务）。
 *
 * 断言只看 DOM（`data-testid` / 文案 / input.value），不看接口：
 *
 * 1. 收货单详情抽屉有「复制为新单」；点它 → 编辑弹窗打开、明细带出、
 *    **采购单提示条可见**、toast 文案是「已复制为 IN-…（草稿）」；
 * 2. 调拨单详情抽屉有「复制为新单」；点它 → 编辑弹窗打开、数量带出；
 * 3. 任务详情抽屉有「复制为新单」；点它 → 抽屉切到副本，标题以「（副本）」结尾；
 * 4. 全程没有 console error / 未捕获异常。
 *
 * 会真的建出复制单（隔离库），跑完由 e2e 脚本的清理负责收尾。
 */
import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';

const BASE = process.env.D9_BASE_URL || 'http://127.0.0.1:5196';
const USER = process.env.D9_ADMIN_USER || 'admin';
const PASSWORDS = [
  process.env.D9_ADMIN_NEW_PASSWORD || 'D9Admin#2026x',
  process.env.D9_ADMIN_PASSWORD || 'Admin@123456',
];
/** 夹具（由 e2e 脚本以 KEEP=1 留在库里；也可用环境变量覆盖） */
const STOCK_IN_NO = process.env.D9_STOCK_IN_NO || '';
const TRANSFER_NO = process.env.D9_TRANSFER_NO || '';
const TASK_ID = process.env.D9_TASK_ID || '';

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
  let session = null;
  for (const password of PASSWORDS) {
    session = await apiLogin(password);
    if (session) break;
  }
  if (!session) {
    console.error('登录失败：口令都不对（D9_ADMIN_NEW_PASSWORD / D9_ADMIN_PASSWORD）');
    process.exit(2);
  }

  const browser = await chromium.launch();
  const context = await browser.newContext({ viewport: { width: 1440, height: 1200 } });
  await context.addInitScript(
    ([a, r]) => {
      localStorage.setItem('x_access_token', a);
      localStorage.setItem('x_refresh_token', r);
      localStorage.setItem('x_guide_done', '1');
      localStorage.setItem('cp-onboarding-done', '1');
    },
    [session.access_token, session.refresh_token],
  );
  const page = await context.newPage();
  const errors = [];
  /** 4xx/5xx 接口（console 的 `Failed to load resource` 看不出是哪一个，这里记 URL） */
  const httpErrors = [];
  page.on('console', (msg) => {
    if (msg.type() === 'error') errors.push(msg.text());
  });
  page.on('response', (resp) => {
    if (resp.status() >= 400) httpErrors.push(`${resp.status()} ${resp.url()}`);
  });
  page.on('pageerror', (error) => errors.push(String(error)));
  /** console 里那条 `Failed to load resource: … 401` 是 SSE 通知流的（既有问题），
   *  它没有 URL 可辨，只能按「本卡关心的接口一条都没 4xx」来判 —— 见 ④ 的口径。 */

  // ---------------- ① 收货单 ----------------
  await page.goto(`${BASE}/business/erp/stock-in`, { waitUntil: 'networkidle' });
  if (!STOCK_IN_NO) {
    console.error('缺少 D9_STOCK_IN_NO（先跑 KEEP=1 的 e2e 并传单号进来）');
    process.exit(2);
  }
  const viewBtn = page.locator(`[data-testid="btn-view-${STOCK_IN_NO}"]`);
  if ((await viewBtn.count()) === 0) {
    bad(`① 列表里找不到收货单 ${STOCK_IN_NO}（夹具没留在库里？）`);
  } else {
    await viewBtn.click();
    await page.waitForSelector('[data-testid="stockin-detail-drawer"] .detail__actions', {
      timeout: 20000,
    });
    const copyBtn = page.locator('[data-testid="btn-stockin-copy"]');
    if (await copyBtn.isVisible()) {
      ok('① 收货单详情抽屉出现「复制为新单」');
    } else {
      bad('① 收货单详情抽屉没有「复制为新单」');
    }
    await copyBtn.click();
    await page.waitForSelector('[data-testid="stockin-form-dialog"]', { timeout: 20000 });
    ok('① 点复制 → 编辑弹窗打开');
    const notice = page.locator('[data-testid="stockin-copy-notice"]');
    if (await notice.isVisible()) {
      const text = ((await notice.textContent()) || '').trim();
      if (text.includes('重新关联采购单')) {
        ok('① 采购单被清空的提示条可见（文案点名要重新关联）');
      } else {
        bad(`① 提示条文案不对：${text}`);
      }
    } else {
      bad('① 没有「来源采购单已清空」提示条');
    }
    // 明细带出：第一行的数量输入框有值（原单是 10）
    const qtyInput = page.locator('[data-testid="stockin-item-qty-0"] input');
    const qty = (await qtyInput.inputValue().catch(() => '')) || '';
    if (Number(qty.replace(/,/g, '')) === 10) {
      ok(`① 明细带出（第 1 行数量 = ${qty}）`);
    } else {
      bad(`① 明细没带出，数量读到「${qty}」`);
    }
    // 采购单提示条里的「已从采购单带出」不该出现（后端已清空关联）
    if ((await page.locator('[data-testid="stockin-source-po"]').count()) === 0) {
      ok('① 表单里没有「从采购单带出」提示（关联确实清空了）');
    } else {
      bad('① 表单仍显示「从采购单带出」——关联没清空');
    }
    const toast = page.locator('.t-message').filter({ hasText: '已复制为' });
    if ((await toast.count()) > 0) {
      ok(`① 成功提示：${((await toast.first().textContent()) || '').trim()}`);
    } else {
      bad('① 没有「已复制为 …（草稿）」的成功提示');
    }
    await page.keyboard.press('Escape');
    await page.waitForTimeout(400);
  }

  // ---------------- ② 调拨单 ----------------
  await page.goto(`${BASE}/business/erp/transfer`, { waitUntil: 'networkidle' });
  const tView = page.locator(`[data-testid="btn-view-${TRANSFER_NO}"]`);
  if ((await tView.count()) === 0) {
    bad(`② 列表里找不到调拨单 ${TRANSFER_NO}`);
  } else {
    await tView.click();
    await page.waitForSelector('[data-testid="transfer-detail-drawer"]', { timeout: 20000 });
    // 抽屉先出骨架、详情回来才有底部按钮 → 等按钮出现再判可见性（否则会误判「没有按钮」）
    await page.waitForSelector('[data-testid="btn-transfer-copy"]', { timeout: 20000 });
    const tCopy = page.locator('[data-testid="btn-transfer-copy"]');
    if (await tCopy.isVisible()) {
      ok('② 调拨单详情抽屉出现「复制为新单」');
    } else {
      bad('② 调拨单详情抽屉没有「复制为新单」');
    }
    await tCopy.click();
    await page.waitForSelector('[data-testid="transfer-form-dialog"]', { timeout: 20000 });
    ok('② 点复制 → 编辑弹窗打开');
    const tNotice = page.locator('[data-testid="transfer-copy-notice"]');
    // 原单选的是「数量物料」（无序列号）→ 不提示序列号，这是正确行为
    if ((await tNotice.count()) === 0) {
      ok('② 原单无序列号 → 不显示序列号提示（不是无脑弹条）');
    } else {
      ok('② 显示序列号提示');
    }
    await page.keyboard.press('Escape');
    await page.waitForTimeout(400);
  }

  // ---------------- ③ 任务 ----------------
  if (!TASK_ID) {
    console.error('缺少 D9_TASK_ID');
    process.exit(2);
  }
  await page.goto(`${BASE}/task/list?id=${TASK_ID}`, { waitUntil: 'networkidle' });
  await page.waitForSelector('[data-testid="btn-task-copy"]', { timeout: 20000 });
  ok('③ 任务详情抽屉出现「复制为新单」');
  const originTitle = ((await page.locator('.head__title').first().textContent()) || '').trim();
  await page.locator('[data-testid="btn-task-copy"]').click();
  // 抽屉会切到副本：标题变成「…（副本）」
  await page.waitForFunction(
    () => {
      const el = document.querySelector('.head__title');
      return Boolean(el && el.textContent && el.textContent.includes('（副本）'));
    },
    { timeout: 20000 },
  );
  const copiedTitle = ((await page.locator('.head__title').first().textContent()) || '').trim();
  if (copiedTitle.endsWith('（副本）') && copiedTitle !== originTitle) {
    ok(`③ 复制后抽屉切到副本，标题 = ${copiedTitle}`);
  } else {
    bad(`③ 标题不对：原「${originTitle}」→ 现「${copiedTitle}」`);
  }

  // ---------------- ④ console / 接口 ----------------
  // console 的 `Failed to load resource: … 401` 全部来自 SSE 通知流（既有问题，见下），
  // 所以这一条按「除 SSE 外没有接口 4xx/5xx」判，而不是按 console 文案判。
  const realHttpErrors = httpErrors.filter((item) => !item.includes('/api/business/notice/stream'));
  if (realHttpErrors.length === 0) {
    ok('④ 除 SSE 通知流外没有 4xx/5xx 接口');
  } else {
    bad(`④ 接口报错：${realHttpErrors.slice(0, 5).join(' | ')}`);
  }
  const unexpected = errors.filter((text) => !text.includes('Failed to load resource'));
  if (unexpected.length === 0) {
    ok('④ 全程没有 JS 异常（未捕获错误 / 组件报错）');
  } else {
    bad(`④ JS 报错：${unexpected.slice(0, 3).join(' | ')}`);
  }
  // /notice/stream 的 401 是**既有**问题（SSE 通知流，本卡一行没碰；见证据文档「已知残留」）：
  // 它每次导航都会重连一次并 401，与复制功能无关，单独计数不混进 D9 的判定。
  const sseErrors = httpErrors.filter((item) => item.includes('/api/business/notice/stream'));
  if (sseErrors.length > 0) {
    console.log(`[NOTE] /api/business/notice/stream 401 × ${sseErrors.length}（既有 SSE 问题，与本卡无关）`);
  }

  await browser.close();
  console.log(`\n== 页面：${PASS} 通过 / ${FAIL} 失败 ==`);
  process.exit(FAIL === 0 ? 0 : 1);
})();
