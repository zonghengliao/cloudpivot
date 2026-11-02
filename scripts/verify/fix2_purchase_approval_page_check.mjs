#!/usr/bin/env node
/**
 * P1-FIX-2 真机验收：采购订单详情里的「审批进度」（Playwright，打**隔离实例**前端入口）。
 *
 * 断言只看 DOM（`data-testid` / 文案），不看接口实现：
 *   ① 审批中单据（SUBMITTED / 节点 ACTIVE）→ 详情抽屉有「审批进度」+ 进度条 + 审批人明细；
 *   ② 进度条节点顺序是图顺序（开始 → 金额分级① → 部门主管审批），不是后端 node_key 序；
 *   ③ 节点状态文案：已通过 / 已通过 / 处理中；
 *   ④ 审批人：Henry.Liao 待处理、老六测试号 待处理（真待办行，引擎写的）；
 *   ⑤ 已通过单据：审批人 + 时间 + 意见都显示（超管 SKIPPED 的留痕意见）；
 *   ⑥ 拉不到节点（路由拦截 500）→ 降级成单号链接，详情主体照常可看，不崩；
 *   ⑦ 全程无 console error / 未捕获异常（SSE 401 既有噪声除外）。
 *
 * 用法：
 *   FIX2_BASE_URL=http://127.0.0.1:5197 FIX2_ORDER_NO=PO-20261102-000001 \
 *   FIX2_APPROVED_ORDER_NO=PO-20261031-000002 node scripts/verify/fix2_purchase_approval_page_check.mjs
 */
import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';

const BASE = process.env.FIX2_BASE_URL || '';
const SUBMITTED_NO = process.env.FIX2_ORDER_NO || '';
const APPROVED_NO = process.env.FIX2_APPROVED_ORDER_NO || 'PO-20261031-000002';
const SHOT_DIR = process.env.FIX2_SHOT_DIR || '/opt/cloudpivot/evidence/fix2-purchase-approval';
const USER = process.env.FIX2_USER || 'admin';
const PASS = process.env.FIX2_PASS || 'Admin@123456';

if (!BASE) {
  console.error('缺少 FIX2_BASE_URL');
  process.exit(2);
}

/**
 * 取会话：优先用 `FIX2_TOKEN_FILE` 里已有的 access/refresh（脚本只读，避免反复登录
 * 触发登录限流 —— `LOGIN_RATE_MAX_ATTEMPTS=10 / 60s`，跑几轮验收就会撞上）。
 * 文件格式：第一行 access_token、第二行 refresh_token。
 */
async function session() {
  const file = process.env.FIX2_TOKEN_FILE || '';
  if (file) {
    const { readFileSync } = await import('node:fs');
    const lines = readFileSync(file, 'utf8').trim().split('\n');
    if (lines[0]) {
      return { access_token: lines[0].trim(), refresh_token: (lines[1] || '').trim() };
    }
  }
  const login = await (await fetch(`${BASE}/api/system/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username: USER, password: PASS }),
  })).json();
  if (login.code !== 0) {
    console.error(`登录失败：${JSON.stringify(login).slice(0, 200)}`);
    process.exit(2);
  }
  return login.data;
}

let PASSED = 0;
let FAILED = 0;
const ok = (msg) => {
  PASSED += 1;
  console.log(`[PASS] ${msg}`);
};
const bad = (msg) => {
  FAILED += 1;
  console.log(`[FAIL] ${msg}`);
};
const info = (msg) => console.log(`[INFO] ${msg}`);

const text = async (locator) => ((await locator.textContent()) || '').replace(/\s+/g, ' ').trim();

(async () => {
  const sess = await session();

  const browser = await chromium.launch();
  const context = await browser.newContext({ viewport: { width: 1440, height: 1100 } });
  await context.addInitScript(
    ([a, r]) => {
      localStorage.setItem('x_access_token', a);
      localStorage.setItem('x_refresh_token', r);
      localStorage.setItem('x_guide_done', '1');
      localStorage.setItem('cp-onboarding-done', '1');
    },
    [sess.access_token, sess.refresh_token],
  );
  const page = await context.newPage();
  const errors = [];
  page.on('console', (msg) => {
    if (msg.type() !== 'error') return;
    const line = msg.text();
    // 既有噪声：通知 SSE 在 test profile 下 401，与本卡无关
    if (line.includes('Failed to load resource')) return;
    errors.push(line);
  });
  page.on('pageerror', (error) => errors.push(String(error)));

  const openDetail = async (orderNo) => {
    await page.goto(`${BASE}/business/erp/purchase`, { waitUntil: 'networkidle' });
    const viewBtn = page.locator(`[data-testid="btn-view-${orderNo}"]`);
    if ((await viewBtn.count()) === 0) {
      return false;
    }
    await viewBtn.click();
    await page.waitForSelector('[data-testid="purchase-detail-drawer"] .detail__grid', {
      timeout: 25000,
    });
    // 抽屉里「审批进度」是异步拉回来的：等侧边栏出现（或降级块出现）再断言
    await page
      .waitForSelector(
        '[data-testid="purchase-detail-approval-progress"], [data-testid="purchase-detail-approval-fallback"]',
        { timeout: 25000 },
      )
      .catch(() => {});
    return true;
  };

  // ---------------- ① 审批中单据：进度条 + 审批人明细 ----------------
  if (!SUBMITTED_NO) {
    console.error('缺少 FIX2_ORDER_NO（先跑 seed_submitted_po.py 拿单号）');
    process.exit(2);
  }
  if (!(await openDetail(SUBMITTED_NO))) {
    bad(`① 列表里找不到采购单 ${SUBMITTED_NO}`);
  } else {
    const progress = page.locator('[data-testid="purchase-detail-approval-progress"]');
    if (await progress.isVisible()) {
      ok('① 审批中单据的详情抽屉出现「审批进度」进度条');
    } else {
      bad('① 详情抽屉没有审批进度进度条（还是只有单号链接？）');
    }

    const title = page.locator('.section-title', { hasText: '审批进度' });
    if ((await title.count()) > 0) {
      ok(`① 区块标题：${await text(title.first())}`);
    } else {
      bad('① 没有「审批进度」区块标题');
    }

    // ② 节点顺序（图顺序，不是 node_key 序）
    const names = await progress.locator('.flow-progress__name').allTextContents();
    info(`节点顺序：${names.map((n) => n.trim()).join(' → ')}`);
    const expected = ['开始', '金额分级①（< 1 万）', '部门主管审批'];
    if (JSON.stringify(names.map((n) => n.trim())) === JSON.stringify(expected)) {
      ok(`② 进度条顺序 = 图顺序（${expected.join(' → ')}）`);
    } else {
      bad(`② 进度条顺序不对：${names.map((n) => n.trim()).join(' → ')}`);
    }

    // ③ 状态文案
    const statuses = (await progress.locator('.flow-progress__status').allTextContents()).map((s) => s.trim());
    info(`节点状态：${statuses.join(' / ')}`);
    if (JSON.stringify(statuses) === JSON.stringify(['已通过', '已通过', '处理中'])) {
      ok('③ 节点状态文案：已通过 / 已通过 / 处理中');
    } else {
      bad(`③ 节点状态文案不对：${statuses.join(' / ')}`);
    }
    const activeSteps = await progress.locator('.flow-progress__step.is-active').count();
    if (activeSteps === 1) {
      ok('③ 「部门主管审批」是唯一进行中节点（品牌色高亮）');
    } else {
      bad(`③ 进行中节点数量 = ${activeSteps}（应为 1）`);
    }

    // ④ 审批人明细（谁 / 什么状态）
    const tasks = page.locator('[data-testid="flow-progress-tasks"]');
    if (await tasks.isVisible()) {
      const taskText = await text(tasks);
      info(`审批人明细：${taskText.slice(0, 220)}`);
      if (taskText.includes('部门主管审批') && taskText.includes('Henry.Liao') && taskText.includes('老六测试号')) {
        ok('④ 审批人明细列出该节点下的审批人（Henry.Liao / 老六测试号）');
      } else {
        bad(`④ 审批人明细内容不对：${taskText.slice(0, 200)}`);
      }
      const pendingCount = await tasks.locator('.flow-progress__task.is-pending').count();
      if (pendingCount >= 2) {
        ok(`④ 待处理标记正确（${pendingCount} 人待处理）`);
      } else {
        bad(`④ 待处理人数 = ${pendingCount}（至少 2）`);
      }
    } else {
      bad('④ 没有审批人明细（只有一根条，看不到谁在审）');
    }

    // 降级块不该出现（节点拉到了）
    if ((await page.locator('[data-testid="purchase-detail-approval-fallback"]').count()) === 0) {
      ok('④ 节点拉到了 → 不显示降级链接');
    } else {
      bad('④ 节点拉到了却仍显示降级链接');
    }

    await page.screenshot({ path: `${SHOT_DIR}/submitted-detail.png`, fullPage: true });
  }

  // ---------------- ⑤ 已通过单据：审批人 + 时间 + 意见 ----------------
  if (!(await openDetail(APPROVED_NO))) {
    bad(`⑤ 列表里找不到采购单 ${APPROVED_NO}`);
  } else {
    const tasks = page.locator('[data-testid="flow-progress-tasks"]');
    if (await tasks.isVisible()) {
      const taskText = await text(tasks);
      info(`已通过单据明细：${taskText.slice(0, 260)}`);
      const hasWho = taskText.includes('Henry.Liao');
      const hasTime = /\d{4}-\d{2}-\d{2} \d{2}:\d{2}/.test(taskText);
      const hasComment = taskText.includes('内控：制单人/发起人不可自审');
      if (hasWho) ok('⑤ 已通过单据显示审批人姓名'); else bad('⑤ 没有审批人姓名');
      if (hasTime) ok('⑤ 显示审批时间（YYYY-MM-DD HH:mm）'); else bad('⑤ 没有审批时间');
      if (hasComment) ok('⑤ 显示审批意见（SKIPPED 留痕说明）'); else bad('⑤ 没有审批意见');
      const doneCount = await tasks.locator('.flow-progress__task.is-approved').count();
      if (doneCount >= 1) ok(`⑤ 已同意标记数 = ${doneCount}`); else bad('⑤ 没有「已同意」标记');
    } else {
      bad('⑤ 已通过单据没有审批人明细');
    }
    await page.screenshot({ path: `${SHOT_DIR}/approved-detail.png`, fullPage: true });
  }

  // ---------------- ⑥ 降级：拉不到节点 → 单号链接，不崩 ----------------
  await page.route('**/api/business/flow/instance/*', (route) =>
    route.fulfill({ status: 500, contentType: 'application/json', body: '{"code":50000,"msg":"boom"}' }),
  );
  const pageErrorsBefore = errors.length;
  if (!(await openDetail(SUBMITTED_NO))) {
    bad(`⑥ 降级场景：列表里找不到 ${SUBMITTED_NO}`);
  } else {
    const fallback = page.locator('[data-testid="purchase-detail-approval-fallback"]');
    if (await fallback.isVisible()) {
      const fallbackText = await text(fallback);
      info(`降级文案：${fallbackText}`);
      if (fallbackText.includes('FLW-')) {
        ok('⑥ 拉不到节点 → 降级显示单号链接（FLW-…）');
      } else {
        bad(`⑥ 降级块没有单号：${fallbackText}`);
      }
    } else {
      bad('⑥ 拉不到节点时没有降级块');
    }
    const drawerBody = page.locator('[data-testid="purchase-detail-drawer"] .detail__grid');
    if (await drawerBody.isVisible()) {
      ok('⑥ 降级时详情主体照常可看（不是整块错误）');
    } else {
      bad('⑥ 降级时详情主体没了');
    }
    if ((await page.locator('[data-testid="purchase-detail-approval-progress"]').count()) === 0) {
      ok('⑥ 拉不到节点时不画空进度条');
    } else {
      bad('⑥ 拉不到节点却画了进度条');
    }
    await page.screenshot({ path: `${SHOT_DIR}/fallback-detail.png`, fullPage: true });
  }
  const newErrors = errors.slice(pageErrorsBefore);
  if (newErrors.length === 0) {
    ok('⑥ 降级过程没有未捕获异常 / console error');
  } else {
    bad(`⑥ 降级过程出现错误：${newErrors.slice(0, 3).join(' | ')}`);
  }
  await page.unroute('**/api/business/flow/instance/*');

  // ---------------- ⑦ 全程 console 干净 ----------------
  if (errors.length === 0) {
    ok('⑦ 全程无 console error / 未捕获异常');
  } else {
    bad(`⑦ 出现 ${errors.length} 条错误：${errors.slice(0, 3).join(' | ')}`);
  }

  await browser.close();
  console.log(`\n===== PASS ${PASSED} / FAIL ${FAILED} =====`);
  process.exit(FAILED === 0 ? 0 : 1);
})();
