/**
 * P3-07 前端配套真机验收（隔离实例：vite dev :5217 → 后端 :18312 → 库 cloudpivot_p307fe）。
 *
 * 三条用例（`run/setup.sh` 已经把库、账号、开关、三张应付单准备好）：
 *  ①  开关打开 + 审批已通过 + 付款仍「审批中」 → 详情里付款记录出现「执行付款」
 *     → 二次确认 → 执行成功 → 后端 message 提示 + 付款记录变「已通过」+ 未付归零
 *  ②  同一状态下用**审批人本人**的令牌点 → 弹 409 + 66013 的中文明文（前后端都拦）
 *  ③  开关关闭回归：审批通过即自动记账 → 付款记录直接「已通过」，按钮不出现
 *
 * 跑法：`node p307e2e.mjs`（脚本所在目录需软链 repo/frontend/node_modules）
 */
import { chromium } from '@playwright/test';
import { readFileSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';

const RUN = '/root/.hermes/kanban/boards/dev/workspaces/t_41801d7f/run';
const SHOTS = join(RUN, 'shots');
const BASE = 'http://127.0.0.1:5217';
const API = 'http://127.0.0.1:18312/api';

const readJson = (f) => JSON.parse(readFileSync(join(RUN, f), 'utf8'));
const tokenOf = (f) => readFileSync(join(RUN, f), 'utf8').trim();
const A = { name: 'p307.pay（发起付款人）', token: tokenOf('pay.token'), refresh: readJson('u_pay.json').data.refresh_token };
const B = { name: 'p307.mgr（部门主管 = 审批人）', token: tokenOf('mgr.token'), refresh: readJson('u_mgr.json').data.refresh_token };
const ADMIN = { name: 'admin', token: tokenOf('admin.token'), refresh: readJson('login2.json').data.refresh_token };

mkdirSync(SHOTS, { recursive: true });

const results = [];
/** 页面里出现过的 4xx/5xx（诊断用：notice/stream 这类全站既有噪声要能一眼认出来） */
const badResponses = [];
function check(name, ok, detail = '') {
  results.push({ name, ok, detail });
  console.log(`${ok ? 'PASS' : 'FAIL'} | ${name}${detail ? ' | ' + detail : ''}`);
  return ok;
}

async function api(path, { method = 'GET', token, body } = {}) {
  const res = await fetch(`${API}${path}`, {
    method,
    headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const json = await res.json().catch(() => ({}));
  return { status: res.status, json };
}

const EXEC_SEL = (no) => `[data-testid="payable-payment-execute-${no}"]`;
const STATUS_SEL = (no) => `[data-testid="payable-payment-status-${no}"]`;

/** 打开一个已注入令牌的页面（新 context，令牌写在 localStorage） */
async function openPage(browser, user, path) {
  const ctx = await browser.newContext({ viewport: { width: 1600, height: 1000 } });
  await ctx.addInitScript(
    ([t, r]) => {
      localStorage.setItem('x_access_token', t);
      if (r) localStorage.setItem('x_refresh_token', r);
      localStorage.setItem('cp-onboarding-done', '1');
      sessionStorage.setItem('login-unread-shown', '1');
    },
    [user.token, user.refresh],
  );
  const page = await ctx.newPage();
  page.on('response', (res) => {
    const url = res.url();
    if (url.includes('/api/')) {
      if (res.status() >= 400) badResponses.push(`${res.status()} ${res.request().method()} ${url.replace(BASE, '')}`);
    }
  });
  await page.goto(`${BASE}${path}`, { waitUntil: 'domcontentloaded' });
  return { ctx, page };
}

/** 等详情抽屉里的那笔付款渲染出来（等**抽屉打开 + 状态标签有文案**，别只等容器出现） */
async function waitPaymentRow(page, paymentNo) {
  await page.waitForFunction(
    (no) => {
      const opened = document.querySelector('.t-drawer.t-drawer--open');
      const el = document.querySelector(`[data-testid="payable-payment-status-${no}"]`);
      return !!opened && !!el && (el.textContent || '').trim().length > 0;
    },
    paymentNo,
    { timeout: 25000 },
  );
}

/** 点「确认执行」并等 POST /payment/{id}/execute 回执 */
async function clickConfirmExecute(page) {
  const respPromise = page.waitForResponse(
    (r) => r.url().includes('/api/business/erp/payable/payment/') && r.url().endsWith('/execute') && r.request().method() === 'POST',
    { timeout: 20000 },
  );
  await page.getByRole('button', { name: '确认执行', exact: true }).click({ noWaitAfter: true }).catch(() => undefined);
  const resp = await respPromise;
  return { status: resp.status(), body: await resp.json().catch(() => ({})) };
}

/** 抓 3 秒就消失的 toast：轮询 DOM 文案 */
async function toastText(page, keyword) {
  return page
    .waitForFunction(
      (kw) => {
        const el = [...document.querySelectorAll('[class*="t-message"]')].find((n) => (n.textContent || '').includes(kw));
        return el ? el.textContent.trim() : null;
      },
      keyword,
      { timeout: 8000 },
    )
    .then((h) => h.jsonValue())
    .catch(() => null);
}

/** 发起付款 → 审批人通过 → 断言付款仍 PENDING 且账未动（= 待他人执行） */
async function prepareAwaiting(payableId, amount, remark) {
  const pay = await api(`/business/erp/payable/${payableId}/pay`, {
    method: 'POST',
    token: A.token,
    body: { amount, pay_method: '银行转账', remark },
  });
  const instanceId = pay.json?.data?.approval_instance_id;
  const todo = await api('/business/flow/task/todo?page_size=20', { token: B.token });
  const task = (todo.json?.data?.list ?? []).find((t) => t.instance_id === instanceId);
  const approve = await api(`/business/flow/task/${task?.id}/approve`, {
    method: 'POST',
    token: B.token,
    body: { comment: `P307 验收：部门主管同意（${remark}）` },
  });
  const info = await api(`/business/erp/payable/${payableId}/payment-info`, { token: A.token });
  return {
    paymentNo: pay.json?.data?.payment_no,
    payOk: pay.status === 202 && pay.json?.data?.outcome === 'PENDING_APPROVAL',
    approved: approve.json?.data?.instance_status === 'APPROVED',
    info,
    payment: info.json?.data?.payments?.find((p) => p.payment_no === pay.json?.data?.payment_no),
  };
}

const browser = await chromium.launch();

// ============ 准备：AP-1（用例①）与 AP-3（用例②）都造出「审批通过但未执行」 ============
const prep1 = await prepareAwaiting('a3070000-0000-4000-8000-000000000101', '1200.0000', 'P307 用例①（换人执行）');
check('准备①：AP-P307-0001 发起付款 → 202 审批中', prep1.payOk, `payment=${prep1.paymentNo}`);
check('准备①：部门主管审批通过 → 实例 APPROVED', prep1.approved);
check(
  '准备①：审批通过后付款仍「审批中」且账一分没动（approve_execute 生效）',
  prep1.payment?.approval_status === 'PENDING' && prep1.info.json?.data?.payable?.paid_amount === '0.0000',
  `status=${prep1.payment?.approval_status} paid=${prep1.info.json?.data?.payable?.paid_amount} awaiting_executor=${JSON.stringify(prep1.payment?.awaiting_executor)}`,
);

const prep3 = await prepareAwaiting('a3070000-0000-4000-8000-000000000103', '600.0000', 'P307 用例②（审批人自己点）');
check('准备②：AP-P307-0003 发起付款 → 审批通过 → 付款仍审批中', prep3.payOk && prep3.approved && prep3.payment?.approval_status === 'PENDING', `payment=${prep3.paymentNo}`);

const PM1 = prep1.paymentNo;
const PM3 = prep3.paymentNo;

// ============ 用例①：开关打开 → 详情出现「执行付款」→ 执行成功 ============
{
  const { ctx, page } = await openPage(browser, A, '/business/erp/payable?payable_no=AP-P307-0001');
  await waitPaymentRow(page, PM1);

  const btn = page.locator(EXEC_SEL(PM1));
  await btn.waitFor({ state: 'visible', timeout: 20000 });
  check('①-1 详情付款记录出现「执行付款」按钮', await btn.isVisible(), `text=${(await btn.innerText()).trim()}`);

  const hint = page.locator('[data-testid="payable-pay-blocked-awaiting-executor"]');
  check(
    '①-2 动作条提示「钱还没付出去 / 找人执行」（不是「等审批结果」）',
    (await hint.count()) > 0 && (await hint.innerText()).includes('钱还没付出去'),
    (await hint.count()) ? (await hint.innerText()).replace(/\s+/g, ' ').trim() : '(missing)',
  );

  const beforeTag = (await page.locator(STATUS_SEL(PM1)).textContent()).trim();
  await page.screenshot({ path: join(SHOTS, '01-awaiting-executor-button.png') });

  await btn.click();
  const dialog = page.locator('.t-dialog', { hasText: '执行付款' }).last();
  await dialog.waitFor({ state: 'visible', timeout: 10000 });
  const dlgText = await dialog.innerText();
  check(
    '①-3 二次确认讲清「还没付 / 立即记账 / 审批人不能执行」',
    ['还没付出去', '应付回写', '付款凭证', '审批与执行分离'].every((k) => dlgText.includes(k)),
    dlgText.replace(/\s+/g, ' ').slice(0, 150),
  );
  await page.screenshot({ path: join(SHOTS, '02-confirm-dialog.png') });

  const { status, body } = await clickConfirmExecute(page);
  check('①-4 点「确认执行」→ POST /execute 200 + data.message', status === 200 && body?.code === 0 && typeof body?.data?.message === 'string', `http=${status} msg=${body?.data?.message}`);

  const toast = await toastText(page, '已执行');
  check('①-5 成功提示 = 后端 message（付款已执行：应付回写 + 付款凭证已生成）', !!toast && toast.includes('付款凭证'), String(toast));

  await page
    .waitForFunction((sel) => document.querySelector(sel)?.textContent.includes('已通过'), STATUS_SEL(PM1), { timeout: 20000 })
    .catch(() => undefined);
  const afterTag = (await page.locator(STATUS_SEL(PM1)).textContent()).trim();
  const unpaid = (await page.locator(`[data-testid="payable-detail-unpaid"]`).textContent()).trim();
  const btnLeft = await page.locator(EXEC_SEL(PM1)).count();
  check('①-6 付款记录「审批中」→「已通过」', beforeTag === '审批中' && afterTag === '已通过', `${beforeTag} → ${afterTag}`);
  check('①-7 应付未付 1200 → 0（账真的动了）', unpaid.includes('0.00') && !unpaid.includes('1,200'), `unpaid=${unpaid}`);
  check('①-8 执行后按钮消失', btnLeft === 0, `count=${btnLeft}`);
  await page.screenshot({ path: join(SHOTS, '03-executed.png') });
  await ctx.close();
}

// ============ 用例②：审批人本人点 → 409 + 66013 中文明文 ============
{
  const { ctx, page } = await openPage(browser, B, '/business/erp/payable?payable_no=AP-P307-0003');
  await waitPaymentRow(page, PM3);
  const btn = page.locator(EXEC_SEL(PM3));
  await btn.waitFor({ state: 'visible', timeout: 20000 });
  check('②-1 审批人自己也能看到按钮（他有付款权限）', await btn.isVisible(), PM3);

  await btn.click();
  await page.locator('.t-dialog', { hasText: '执行付款' }).last().waitFor({ state: 'visible', timeout: 10000 });
  const { status, body } = await clickConfirmExecute(page);
  check('②-2 后端回 409 + 66013', status === 409 && body?.code === 66013, `http=${status} code=${body?.code}`);

  const toast = await toastText(page, '审批与执行分离');
  check('②-3 弹的是后端 msg 原文（中文明文，一字未改）', !!toast && toast.includes('不能自己执行') && toast.includes('approve_execute'), String(toast));
  await page.screenshot({ path: join(SHOTS, '04-66013-toast.png') });

  const after = await api('/business/erp/payable/a3070000-0000-4000-8000-000000000103/payment-info', { token: A.token });
  check(
    '②-4 拦下后账一分没动（付款仍审批中 / paid=0）',
    after.json?.data?.payments?.[0]?.approval_status === 'PENDING' && after.json?.data?.payable?.paid_amount === '0.0000',
    `status=${after.json?.data?.payments?.[0]?.approval_status} paid=${after.json?.data?.payable?.paid_amount}`,
  );
  await ctx.close();
}

// ============ 用例③：开关关闭回归（审批通过即已生效） ============
{
  const CID = 'e2b00000-0000-4000-8000-000000000006';
  const cfg = await api(`/support/config/${CID}`, { token: ADMIN.token });
  const payloadBody = { ...cfg.json.data, config_value: 'false' };
  const off = await api(`/support/config/${CID}`, { method: 'PUT', token: ADMIN.token, body: payloadBody });
  check('③-1 系统参数关掉 approve_execute', off.json?.data?.config_value === 'false', `value=${off.json?.data?.config_value}`);

  const info = await prepareAwaiting('a3070000-0000-4000-8000-000000000102', '800.0000', 'P307 用例③（开关关闭）');
  const pmt = info.payment;
  check(
    '③-2 审批通过即自动记账：付款直接「已通过」+ paid=800',
    pmt?.approval_status === 'APPROVED' && info.info.json?.data?.payable?.paid_amount === '800.0000',
    `status=${pmt?.approval_status} paid=${info.info.json?.data?.payable?.paid_amount}`,
  );

  const PM2 = pmt?.payment_no;
  const { ctx, page } = await openPage(browser, A, '/business/erp/payable?payable_no=AP-P307-0002');
  await waitPaymentRow(page, PM2);
  const btnLeft = await page.locator(EXEC_SEL(PM2)).count();
  const tag = (await page.locator(STATUS_SEL(PM2)).textContent()).trim();
  check('③-3 开关关闭时「执行付款」按钮不出现（回归：审批通过即已生效）', btnLeft === 0 && tag === '已通过', `button=${btnLeft} tag=${tag}`);
  await page.screenshot({ path: join(SHOTS, '05-switch-off-no-button.png') });
  await ctx.close();

  // 复原开关（留在开启态，方便人工复核）
  await api(`/support/config/${CID}`, { method: 'PUT', token: ADMIN.token, body: { ...payloadBody, config_value: 'true' } });
}

await browser.close();

const fails = results.filter((r) => !r.ok);
console.log(`\n===== P3-07 前端验收：PASS ${results.length - fails.length} / FAIL ${fails.length} =====`);
if (badResponses.length) {
  console.log('--- 页面里的 4xx/5xx（诊断）---');
  console.log([...new Set(badResponses)].join('\n'));
}
if (fails.length) {
  for (const f of fails) console.log(`FAILED: ${f.name} | ${f.detail}`);
  process.exit(1);
}
