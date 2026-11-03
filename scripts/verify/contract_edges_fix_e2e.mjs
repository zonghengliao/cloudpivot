/**
 * 【P1-1-FIX】合同前端边表跟上后端状态机 —— 真机回归（生产实例，真实 UI + 真实库）
 *
 * 要证的事（对应卡面 第 4 步）：
 *   ① `DRAFT` 合同的**列表行快捷动作**（走后端没下发 next_statuses 的回落路径）给的是
 *      「提交审批」，**不再**给「登记签订」（后端已删 `DRAFT → SIGNED`，点了会 409）；
 *   ② 详情抽屉出现「提交审批」而不是「签订」；
 *   ③ 提交后状态变「审批中」，详情再出现「审批通过签订 / 驳回撤回」；
 *   ④ 驳回必须填原因（流程待办 reject 意见 ≥ 5 字，空意见 400）；
 *   ⑤ 驳回后合同回到「草稿」，收尾把测试草稿删掉（不在生产留垃圾单）。
 *
 * ⚠️ 本脚本**不会**审批掉任何生产合同：只建一张一次性草稿 → 提交 → 驳回 → 删除。
 *
 * 用法（在能连到生产机的机器上；playwright 浏览器需已安装）：
 *   cd /opt/cloudpivot/frontend
 *   DC_BASE=http://127.0.0.1 DC_PASS='<admin 口令>' \
 *     node ../scripts/verify/contract_edges_fix_e2e.mjs
 *
 * 环境变量：
 *   DC_BASE      生产前端地址，默认 http://127.0.0.1（在 144 上跑时）
 *   DC_OUT       截图目录，默认 <repo>/docs/evidence/20261103-contract-edges-fix/shots
 *   DC_USER      admin
 *   DC_PASS      admin 口令（/etc/cloudpivot/cloudpivot.env 的 NIK_ADMIN_PASSWORD）
 *   DC_KEEP=1    保留测试草稿（默认删除）
 */
import { mkdirSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const REPO = resolve(HERE, '../..');
const FE_DIR = process.env.DC_FE_DIR ?? `${REPO}/frontend`;
const { chromium } = createRequire(`${FE_DIR}/package.json`)('@playwright/test');

const BASE = (process.env.DC_BASE ?? 'http://127.0.0.1').replace(/\/$/, '');
const OUT =
  process.env.DC_OUT ?? `${REPO}/docs/evidence/20261103-contract-edges-fix/shots`;
const USER = process.env.DC_USER ?? 'admin';
const PASS = process.env.DC_PASS ?? '';
const KEEP = (process.env.DC_KEEP ?? '0') === '1';

if (!PASS) {
  console.error('缺 DC_PASS（/etc/cloudpivot/cloudpivot.env 的 NIK_ADMIN_PASSWORD）');
  process.exit(2);
}
mkdirSync(OUT, { recursive: true });

const results = [];
const shots = [];
const consoleErrors = [];
const httpErrors = [];
const IGNORED_HTTP = []; // 预期内的负向断言（如驳回空意见 400）

function rec(name, passed, detail = '') {
  results.push({ name, passed, detail });
  console.log(`${passed ? 'PASS' : 'FAIL'} | ${name}${detail ? ' | ' + detail : ''}`);
  return passed;
}
async function shot(page, name) {
  const path = `${OUT}/${name}.png`;
  await page.screenshot({ path, fullPage: false });
  shots.push(path);
  console.log(`[shot] ${path}`);
  return path;
}
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** 行内 t-link 有时被表格滚动/动画卡住「不稳定」→ 先滚进视口再 force 点 */
async function safeClick(locator, label = '') {
  try {
    await locator.scrollIntoViewIfNeeded({ timeout: 5000 });
  } catch {
    /* 已经可见就算了 */
  }
  await locator.click({ force: true, timeout: 15000 }).catch(async (error) => {
    console.log(`[safeClick] ${label} 常规点击失败，重试一次：${String(error).slice(0, 120)}`);
    await sleep(500);
    await locator.click({ force: true, timeout: 15000 });
  });
}

/** 直接打后端（走 nginx 的 /api 前缀），脚本里负责建单 / 断言 / 收尾 */
let TOKEN = '';
async function api(method, path, body, { raw = false } = {}) {
  const res = await fetch(`${BASE}/api${path}`, {
    method,
    headers: {
      'Content-Type': 'application/json',
      ...(TOKEN ? { Authorization: `Bearer ${TOKEN}` } : {}),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await res.text();
  let json = null;
  try {
    json = text ? JSON.parse(text) : null;
  } catch {
    /* 非 JSON 原样返回 */
  }
  if (raw) return { status: res.status, json, text };
  if (res.status >= 400) {
    const err = new Error(`${method} ${path} → ${res.status} ${text.slice(0, 300)}`);
    err.status = res.status;
    err.json = json;
    throw err;
  }
  return json?.data ?? json;
}

async function login() {
  const res = await api('POST', '/system/auth/login', { username: USER, password: PASS }, { raw: true });
  const data = res.json?.data ?? {};
  if (res.status !== 200 || !data.access_token) {
    throw new Error(`登录失败 ${res.status}: ${res.text.slice(0, 300)}`);
  }
  if (data.must_change_password) {
    throw new Error('admin 是首登态（must_change_password），脚本不改生产口令 —— 先人工改密再跑');
  }
  TOKEN = data.access_token;
  return data;
}

/** 建一张一次性草稿合同 */
async function createDraftContract(tag) {
  const page = await api('GET', '/business/erp/supplier/page?page=1&page_size=1');
  const supplier = (page?.list ?? [])[0];
  if (!supplier) throw new Error('生产库没有可用供应商，无法建合同草稿');
  const today = new Date(Date.now() + 8 * 3600 * 1000).toISOString().slice(0, 10);
  const created = await api('POST', '/business/erp/contract', {
    name: `P1-1-FIX 回归草稿 ${tag}（自动清理）`,
    contract_type: 'PURCHASE',
    party_b_id: supplier.id,
    amount: 1000,
    currency_code: 'CNY',
    sign_date: today,
    start_date: today,
    end_date: '2027-12-31',
    remark: 'P1-1-FIX 前端边表回归用，脚本收尾自动删除',
  });
  return { contract: created, supplierName: supplier.supplier_name ?? supplier.name ?? '' };
}

async function contractDetail(id) {
  return api('GET', `/business/erp/contract/${id}`);
}

/** 等合同状态变成期望值（驳回回调 / 提交起流程都是异步落库的） */
async function waitStatus(id, want, timeoutMs = 20000) {
  const deadline = Date.now() + timeoutMs;
  let last = '';
  while (Date.now() < deadline) {
    const detail = await contractDetail(id);
    last = String(detail.status ?? '');
    if (last === want) return last;
    await sleep(700);
  }
  throw new Error(`等 ${want} 超时，最后一次状态 = ${last}`);
}

/** 找到这张合同审批实例上「PENDING 的待办」+ 审批人（admin 未必是审批人：V0209 派给 ROLE） */
async function findPendingTask(contractId) {
  const detail = await contractDetail(contractId);
  const instanceId = detail.approval_instance_id ?? detail.instance_id ?? '';
  if (!instanceId) return { instanceId: '', pending: null, tasks: [] };
  const inst = await api('GET', `/business/flow/instance/${instanceId}`);
  const tasks = (inst?.timeline ?? []).filter(
    (task) => String(task.task_kind ?? '').toUpperCase() !== 'RESUBMIT',
  );
  const pending = tasks.find((task) => String(task.status ?? '').toUpperCase() === 'PENDING') ?? null;
  return { instanceId, pending, tasks };
}

// ============================================================ 主流程

const browser = await chromium.launch({ args: ['--no-sandbox'] });
const ctx = await browser.newContext({ viewport: { width: 1500, height: 950 } });
const page = await ctx.newPage();

page.on('console', (msg) => {
  if (msg.type() === 'error') consoleErrors.push(msg.text().slice(0, 300));
});
page.on('pageerror', (err) => consoleErrors.push(`pageerror: ${String(err).slice(0, 300)}`));
page.on('response', (res) => {
  const url = res.url();
  if (res.status() >= 400 && !url.includes('/favicon') && !IGNORED_HTTP.some((s) => url.includes(s))) {
    httpErrors.push(`${res.status()} ${res.request().method()} ${url.replace(BASE, '')}`);
  }
});

const TAG = new Date(Date.now() + 8 * 3600 * 1000)
  .toISOString()
  .replace(/[-:T]/g, '')
  .slice(0, 14);
let contract = null;

try {
  // ---------------------------------------------------------------- 0 建草稿（API）
  console.log('\n=== 0 建一张一次性草稿合同（收尾删除） ===');
  await login();
  const picked = await createDraftContract(TAG);
  contract = picked.contract;
  const NO = contract.contract_no;
  rec('⓪-1 建出草稿合同', contract.status === 'DRAFT', `${NO} status=${contract.status} 乙方=${picked.supplierName}`);

  // ---------------------------------------------------------------- 1 登录 + 列出这张草稿
  console.log('\n=== 1 列表行快捷动作（后端未下发 next_statuses 的回落路径） ===');
  await page.goto(`${BASE}/login`, { waitUntil: 'domcontentloaded', timeout: 45000 });
  await page.waitForSelector('input[placeholder="请输入用户名"]', { timeout: 30000 });
  await page.fill('input[placeholder="请输入用户名"]', USER);
  await page.fill('input[placeholder="请输入密码"]', PASS);
  await page.click('button:has-text("登")');
  await page.waitForURL(/dashboard|workbench|\/$/, { timeout: 30000 }).catch(() => {});
  await sleep(2000);

  await page.goto(`${BASE}/business/erp/contract`, { waitUntil: 'domcontentloaded', timeout: 45000 });
  await page.waitForSelector('[data-testid="contract-filter-no"]', { timeout: 30000 });
  await page.fill('[data-testid="contract-filter-no"] input', NO);
  await page.click('[data-testid="btn-contract-search"]');
  await sleep(1500);
  await page.waitForSelector(`[data-testid="contract-status-${NO}"]`, { timeout: 20000 });

  const submitBtn = page.locator(`[data-testid="btn-contract-to-PENDING_APPROVAL-${NO}"]`);
  const signedBtn = page.locator(`[data-testid="btn-contract-to-SIGNED-${NO}"]`);
  const submitCount = await submitBtn.count();
  const signedCount = await signedBtn.count();
  const submitText = submitCount ? (await submitBtn.first().innerText()).trim() : '';
  await shot(page, '01-list-draft-quick-action');

  rec('①-1 列表行给出「提交审批」', submitCount > 0 && submitText.includes('提交审批'), `按钮=${submitCount} 文案=${submitText}`);
  rec('①-2 列表行不再给出「签订」(DRAFT→SIGNED 已删)', signedCount === 0, `DRAFT→SIGNED 按钮数=${signedCount}`);

  // ---------------------------------------------------------------- 2 详情抽屉：DRAFT 的按钮
  console.log('\n=== 2 详情抽屉：DRAFT 只该有「提交审批」 ===');
  // 用页面自带的深链 ?contract_id=（A6 加的入口）直开抽屉：比在表格里点「查看」稳，
  // 也顺手验证这个入口本身没坏
  await page.goto(`${BASE}/business/erp/contract?contract_id=${contract.id}`, {
    waitUntil: 'domcontentloaded',
    timeout: 45000,
  });
  await page.waitForSelector('[data-testid="contract-detail"]', { timeout: 25000 });
  await page.waitForSelector('[data-testid="contract-detail-actions"]', { timeout: 20000 });
  await sleep(500);
  const dSubmit = page.locator('[data-testid="btn-contract-detail-to-PENDING_APPROVAL"]');
  const dSigned = page.locator('[data-testid="btn-contract-detail-to-SIGNED"]');
  const dSubmitCount = await dSubmit.count();
  const dSignedCount = await dSigned.count();
  const dSubmitText = dSubmitCount ? (await dSubmit.first().innerText()).trim() : '';
  await page.locator('[data-testid="contract-detail-actions"]').first().scrollIntoViewIfNeeded().catch(() => {});
  await sleep(300);
  await shot(page, '02-detail-draft-submit-approval');
  rec('②-1 详情出现「提交审批」', dSubmitCount > 0 && dSubmitText.includes('提交审批'), `文案=${dSubmitText}`);
  rec('②-2 详情没有「登记签订」按钮', dSignedCount === 0, `detail→SIGNED 按钮数=${dSignedCount}`);

  // ---------------------------------------------------------------- 3 点「提交审批」→ 审批中
  console.log('\n=== 3 提交审批 → 状态变「审批中」 ===');
  await safeClick(dSubmit.first(), 'detail-submit');
  await page.waitForSelector('[data-testid="contract-status-dialog"]', { timeout: 20000 });
  await sleep(500);
  const confirmText = (await page.locator('[data-testid="contract-status-confirm-text"]').innerText()).trim();
  const reasonBoxes = await page.locator('[data-testid="contract-status-reason"]').count();
  await shot(page, '03-confirm-dialog-submit-approval');
  rec('③-1 确认弹窗文案是「草稿 → 审批中」', confirmText.includes('草稿') && confirmText.includes('审批中'), confirmText.slice(0, 60));
  rec('③-2 提交审批不需要填原因（后端 CONTRACT_REASON_EDGES 只有两条终止边）', reasonBoxes === 0, `原因框=${reasonBoxes}`);

  await page.locator('[data-testid="contract-status-dialog"] button:has-text("确认")').last().click();
  await page.waitForSelector('[data-testid="contract-status-dialog"]', { state: 'hidden', timeout: 20000 }).catch(() => {});
  const afterStatus = await waitStatus(contract.id, 'PENDING_APPROVAL', 30000);
  await sleep(1200);
  rec('③-3 提交后后端状态 = PENDING_APPROVAL', afterStatus === 'PENDING_APPROVAL', `status=${afterStatus}`);

  // 列表徽标 + 详情按钮（抽屉还开着，提交后已刷新 detail）
  await page.waitForSelector('[data-testid="btn-contract-detail-to-SIGNED"]', { timeout: 20000 });
  await sleep(400);
  await page.locator('[data-testid="contract-detail-actions"]').first().scrollIntoViewIfNeeded().catch(() => {});
  await sleep(300);
  await shot(page, '04-detail-pending-approval-actions');
  const dBadge = page.locator('[data-testid="contract-detail-status"]').first();
  const badgeText = (await dBadge.innerText().catch(() => '')).trim();
  const pApprove = page.locator('[data-testid="btn-contract-detail-to-SIGNED"]');
  const pReject = page.locator('[data-testid="btn-contract-detail-to-DRAFT"]');
  const pApproveText = (await pApprove.first().innerText().catch(() => '')).trim();
  const pRejectText = (await pReject.first().innerText().catch(() => '')).trim();
  rec('③-4 合同状态徽标显示「审批中」', /审批中|PENDING_APPROVAL/.test(badgeText), `徽标=${badgeText}`);
  rec('③-5 审批中详情出现「审批通过签订」', (await pApprove.count()) > 0 && pApproveText.includes('审批通过'), `文案=${pApproveText}`);
  rec('③-6 审批中详情出现「驳回/撤回」', (await pReject.count()) > 0 && pRejectText.includes('驳回'), `文案=${pRejectText}`);

  // 关抽屉，回到列表：列表行快捷动作也应该是这两条
  await safeClick(page.locator('[data-testid="btn-contract-detail-close"]'), 'close-detail');
  await sleep(800);
  await safeClick(page.locator('[data-testid="btn-contract-refresh"]'), 'refresh');
  await sleep(1500);
  const rowApprove = page.locator(`[data-testid="btn-contract-to-SIGNED-${NO}"]`);
  const rowReject = page.locator(`[data-testid="btn-contract-to-DRAFT-${NO}"]`);
  rec('③-7 列表行（审批中）也给「审批通过签订 / 驳回撤回」',
    (await rowApprove.count()) > 0 && (await rowReject.count()) > 0,
    `SIGNED=${await rowApprove.count()} DRAFT=${await rowReject.count()}`);
  await shot(page, '05-list-pending-approval');

  // ---------------------------------------------------------------- 4 驳回必须填原因（真实待办）
  console.log('\n=== 4 驳回意见必填（对真实待办打真接口）===');
  const found = await findPendingTask(contract.id);
  rec('④-1 审批中合同挂了审批实例', Boolean(found.instanceId), `instance=${found.instanceId || '（空）'}`);
  rec('④-2 实例上有 PENDING 审批待办', Boolean(found.pending),
    found.pending
      ? `节点=${found.pending.current_node_name} 审批人=${found.pending.assignee_name}（${found.pending.assignee_source}）`
      : `timeline 任务数=${found.tasks.length}`);

  if (found.pending) {
    IGNORED_HTTP.push(`/business/flow/task/${found.pending.id}/reject`);
    const empty = await api('POST', `/business/flow/task/${found.pending.id}/reject`, { comment: '' }, { raw: true });
    rec('④-3 驳回空意见被拒（400 + 65005「驳回必须填写意见」）', empty.status === 400,
      `${empty.status} ${(empty.text ?? '').slice(0, 90)}`);

    const short = await api('POST', `/business/flow/task/${found.pending.id}/reject`, { comment: '不好' }, { raw: true });
    rec('④-4 驳回 2 字意见被拒（至少 5 字）', short.status === 400 && /5/.test(short.text ?? ''),
      `${short.status} ${(short.text ?? '').slice(0, 90)}`);

    const crossUser = await api(
      'POST',
      `/business/flow/task/${found.pending.id}/reject`,
      { comment: 'P1-1-FIX 回归：验证驳回意见必填（admin 代打，预期被越权校验挡住）' },
      { raw: true },
    );
    rec(`④-5 admin 不是这条待办的审批人（应 403；审批人=${found.pending.assignee_name}）`,
      crossUser.status === 403, `${crossUser.status} ${(crossUser.text ?? '').slice(0, 90)}`);
  }

  // ---------------------------------------------------------------- 4b 前端「驳回/撤回」→ 回草稿
  console.log('\n=== 4b 前端点「驳回/撤回」（PENDING_APPROVAL→DRAFT）→ 合同回草稿 ===');
  await page.goto(`${BASE}/business/erp/contract?contract_id=${contract.id}`, {
    waitUntil: 'domcontentloaded',
    timeout: 45000,
  });
  await page.waitForSelector('[data-testid="btn-contract-detail-to-DRAFT"]', { timeout: 25000 });
  await page.locator('[data-testid="contract-detail-actions"]').first().scrollIntoViewIfNeeded().catch(() => {});
  await sleep(300);
  await shot(page, '06-detail-reject-withdraw-button');
  await safeClick(page.locator('[data-testid="btn-contract-detail-to-DRAFT"]').first(), 'detail-reject');
  await page.waitForSelector('[data-testid="contract-status-dialog"]', { timeout: 20000 });
  await sleep(400);
  const withdrawConfirm = (await page
    .locator('[data-testid="contract-status-confirm-text"]')
    .innerText()).trim();
  await shot(page, '07-confirm-dialog-reject-withdraw');
  rec('④-6 「驳回/撤回」确认弹窗说明了副作用（终止审批实例 / 回草稿）',
    withdrawConfirm.includes('审批实例') && withdrawConfirm.includes('草稿'), withdrawConfirm.slice(0, 80));

  await page.locator('[data-testid="contract-status-dialog"] button:has-text("确认")').last().click();
  await page.waitForSelector('[data-testid="contract-status-dialog"]', { state: 'hidden', timeout: 20000 }).catch(() => {});
  const backStatus = await waitStatus(contract.id, 'DRAFT', 30000);
  rec('④-7 驳回/撤回后后端状态回到 DRAFT', backStatus === 'DRAFT', `status=${backStatus}`);

  await safeClick(page.locator('[data-testid="btn-contract-detail-close"]'), 'close-detail');
  await sleep(800);
  await safeClick(page.locator('[data-testid="btn-contract-refresh"]'), 'refresh-after-reject');
  await sleep(1500);
  const backBadge = (await page.locator(`[data-testid="contract-status-${NO}"]`).first().innerText().catch(() => '')).trim();
  const submitAgain = await page.locator(`[data-testid="btn-contract-to-PENDING_APPROVAL-${NO}"]`).count();
  await shot(page, '08-list-rejected-back-to-draft');
  rec('④-8 列表回到「草稿」且又能「提交审批」', /草稿|DRAFT/.test(backBadge) && submitAgain > 0,
    `徽标=${backBadge} 提交审批按钮=${submitAgain}`);

  // ---------------------------------------------------------------- 5 收尾：删掉测试草稿
  console.log('\n=== 5 收尾：删除测试草稿 ===');
  if (KEEP) {
    rec('⑤-1 保留草稿（DC_KEEP=1）', true, NO);
  } else {
    const del = await api('DELETE', `/business/erp/contract/${contract.id}`, undefined, { raw: true });
    rec('⑤-1 测试草稿已删除', del.status === 200, `${del.status} ${NO}`);
  }

  // ---------------------------------------------------------------- 6 控制台 / 网络
  const unexpected = httpErrors.filter((line) => !line.includes('/business/flow/task/'));
  rec('⑥-1 无意外 4xx/5xx', unexpected.length === 0, unexpected.slice(0, 3).join(' ; ') || 'none');
  rec('⑥-2 无前端 console error', consoleErrors.length === 0, consoleErrors.slice(0, 2).join(' ; ') || 'none');
} catch (error) {
  rec('✗ 异常中止', false, String(error).slice(0, 400));
  // 失败也尽量不留垃圾单
  if (contract && !KEEP) {
    try {
      await api('DELETE', `/business/erp/contract/${contract.id}`);
      console.log(`[cleanup] 已删除 ${contract.contract_no}`);
    } catch (cleanupError) {
      console.log(`[cleanup] 删除失败（合同可能已不是草稿）：${String(cleanupError).slice(0, 200)}`);
    }
  }
} finally {
  await browser.close();
}

const passed = results.filter((r) => r.passed).length;
const failed = results.length - passed;
const summary = {
  base: BASE,
  contract_no: contract?.contract_no ?? null,
  tests: results.length,
  passed,
  failed,
  shots,
  console_errors: consoleErrors,
  http_errors: httpErrors,
  results,
};
writeFileSync(`${OUT}/../result.json`, JSON.stringify(summary, null, 2));
console.log(`\n=== 汇总：${passed}/${results.length} PASS，${failed} FAIL；截图 ${shots.length} 张 ===`);
process.exit(failed === 0 ? 0 : 1);
