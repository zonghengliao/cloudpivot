/**
 * HOME-5 验收（Playwright / Chromium）：工作台「待我审批」卡。
 *
 * 覆盖卡面验收：
 *   ① 工作台有卡  ② 最多 10 条 + 底部真实总数  ③ 类型/单号/提交人/相对时间/金额
 *   ④ 同意 → 确认框 → 接口 → 该条消失 + Toast
 *   ⑤ 驳回 → 必须填意见（<5 字按钮禁用）→ 接口 → 该条消失
 *   ⑥ 加签 → 选人 → 接口（该条保留）
 *   ⑦ 点单号按 biz_type 跳业务详情页
 *   ⑧ 无待办空态  ⑨ 非超管也能看到（自己的待办）
 *
 * 运行：node /var/tmp/home5/verify-ui.mjs
 */
import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';
import { mkdirSync, writeFileSync } from 'node:fs';

const BASE = process.env.BASE || 'http://127.0.0.1:5401';
const OUT = '/var/tmp/home5/ui';
const ADMIN = { username: 'admin', password: 'Home5@2026Pass' };
const VIEWER = { username: 'h5viewer', password: 'Viewer@2026Pass' };

mkdirSync(OUT, { recursive: true });
const report = { pass: [], fail: [], shots: [], details: {} };
const ok = (cond, msg, extra = '') => {
  console.log(`${cond ? 'PASS' : 'FAIL'}  ${msg}${extra ? ' :: ' + extra : ''}`);
  (cond ? report.pass : report.fail).push(msg);
  if (extra) report.details[msg] = extra;
  return cond;
};
const shots = async (page, name) => {
  const path = `${OUT}/${name}.png`;
  await page.screenshot({ path, fullPage: true });
  report.shots.push(path);
  console.log(`  📸 ${path}`);
};

async function login(page, who) {
  await page.goto(`${BASE}/login`, { waitUntil: 'domcontentloaded' });
  await page.getByPlaceholder('请输入用户名').fill(who.username);
  await page.getByPlaceholder('请输入密码').fill(who.password);
  await page.locator('button[type="submit"]').first().click();
  await page.waitForURL(/\/dashboard/, { timeout: 30000 });
  await page.waitForTimeout(1800);
  for (const testid of ['setup-wizard-skip', 'login-unread-ok']) {
    const btn = page.getByTestId(testid);
    if (await btn.count()) {
      await btn.first().click({ timeout: 5000 }).catch(() => {});
      await page.waitForTimeout(600);
    }
  }
}

/** 卡片当前状态：行数 / 底部统计文本 */
async function cardState(page) {
  const rows = await page.locator('[data-testid^="home-approval-item-"]').count();
  const stats = (await page.getByTestId('home-approval-stats').count())
    ? (await page.getByTestId('home-approval-stats').innerText()).replace(/\s+/g, ' ').trim()
    : '';
  return { rows, stats };
}

const browser = await chromium.launch();
try {
  const ctx = await browser.newContext({ viewport: { width: 1680, height: 1080 } });
  await ctx.addInitScript(() => {
    try { localStorage.setItem('cp-onboarding-done', '1'); } catch { /* ignore */ }
  });
  const page = await ctx.newPage();
  const consoleErrors = [];
  const responses = [];
  page.on('console', (m) => { if (m.type() === 'error') consoleErrors.push(m.text()); });
  page.on('pageerror', (e) => consoleErrors.push(String(e)));
  page.on('response', (r) => responses.push({ url: r.url(), status: r.status() }));

  await login(page, ADMIN);

  // ---------- ① 卡片存在 + ② 行数 / 总数 ----------
  const card = page.getByTestId('home-approval-card');
  ok((await card.count()) === 1, '① 工作台有「待我审批」卡');
  const title = await card.locator('.t-card__title').first().innerText();
  ok(title.includes('待我审批'), '① 卡标题 = 待我审批', title);

  let state = await cardState(page);
  ok(state.rows === 10, '② 待办最多显示 10 条', `rows=${state.rows}`);
  ok(/待办\s*11\s*项/.test(state.stats), '② 底部显示真实待办总数（11）', state.stats);
  ok(/今日已办\s*1\s*项/.test(state.stats), '② 底部显示今日已办（1）', state.stats);
  await shots(page, '01-workbench-approval-card');

  // ---------- ③ 每行内容 ----------
  const texts = await page.locator('[data-testid^="home-approval-item-"]').allInnerTexts();
  report.details.rows = texts.map((t) => t.replace(/\n/g, ' | '));
  ok(texts.some((t) => t.includes('采购订单') && t.includes('PO-20261109-000003')), '③ 第一类行：单据类型 + 业务单号', texts[0]);
  ok(texts[0].includes('张三（演示发起人）'), '③ 提交人取真实 initiator_name', texts[0]);
  ok(/30\s*分钟前/.test(texts[0]), '③ 相对时间（N 分钟前）', texts[0]);
  ok(texts[0].includes('¥12,800'), '③ 金额（¥12,800）', texts[0]);
  ok(/¥150,000/.test(texts.join('\n')), '③ 合同金额（¥150,000）也显示');
  ok(!/¥0/.test(texts.join('\n')), '③ 无金额单据不渲染 ¥0（报销单该行无金额）', '无金额行 = ' + (texts.find((t) => t.includes('EXP-20261106-0004')) || ''));
  ok(!(texts.find((t) => t.includes('EXP-20261106-0004')) || '').includes('¥'), '③ 无金额单据整行无 ¥ 占位');
  ok(texts.some((t) => /2026-11-01/.test(t)), '③ 超过 7 天显示日期（2026-11-01）');
  ok(/2\s*小时前/.test(texts.join('\n')) && /3\s*天前/.test(texts.join('\n')), '③ 小时 / 天 两种相对时间');

  // ---------- ⑦ 点单号跳业务详情（PO_APPROVE → 采购订单） ----------
  const poRow = page.locator('[data-testid^="home-approval-item-"]').filter({ hasText: 'PO-20261109-000003' });
  const poTitle = poRow.locator('.apv__title');
  await poTitle.click();
  await page.waitForURL(/\/business\/erp\/purchase-order\?po_id=/, { timeout: 10000 });
  const poUrl = page.url();
  ok(/\/business\/erp\/purchase-order\?po_id=[0-9a-f-]{36}$/.test(poUrl), '⑦ 采购订单 → /business/erp/purchase-order?po_id=', poUrl);
  await shots(page, '02-doc-jump-purchase-order');
  await page.goBack();
  await page.waitForURL(/\/dashboard/, { timeout: 15000 });
  await page.getByTestId('home-approval-card').waitFor({ state: 'visible', timeout: 20000 });
  await page.locator('[data-testid^="home-approval-item-"]').first().waitFor({ state: 'visible', timeout: 20000 });
  await page.waitForTimeout(800);

  // ---------- ④ 同意（确认框 → 接口 → 消失 + Toast） ----------
  const approveRow = page.locator('[data-testid^="home-approval-item-"]').filter({ hasText: 'PO-20261109-000003' });
  const approveId = await approveRow.getAttribute('data-testid');
  await approveRow.locator('[data-testid^="home-approval-approve-"]').click();
  const approveDialog = page.getByTestId('home-approval-approve-dialog');
  await approveDialog.waitFor({ state: 'visible', timeout: 8000 });
  ok((await approveDialog.innerText()).includes('PO-20261109-000003'), '④ 同意弹确认框并回显目标单据');
  await approveDialog.locator('[data-testid="home-approval-approve-comment"] textarea').fill('验收：同意该采购订单');
  await shots(page, '03-approve-dialog');
  await approveDialog.getByRole('button', { name: '确认同意' }).click();
  await page.locator('.t-message').filter({ hasText: '已同意' }).first().waitFor({ timeout: 12000 });
  ok(true, '④ 同意成功后 Toast「已同意」');
  await page.waitForTimeout(1500);
  ok((await page.locator(`[data-testid="${approveId}"]`).count()) === 0, '④ 该条从列表移除');
  state = await cardState(page);
  ok(/待办\s*10\s*项/.test(state.stats) && /今日已办\s*2\s*项/.test(state.stats), '④ 统计同步：待办 10 / 今日已办 2', state.stats);
  await shots(page, '04-after-approve');

  // ---------- ⑤ 驳回：意见必填（< 5 字按钮禁用）→ 提交 → 消失 ----------
  const rejectRow = page.locator('[data-testid^="home-approval-item-"]').filter({ hasText: 'EXP-20261109-0002' });
  await rejectRow.locator('[data-testid^="home-approval-reject-"]').click();
  const rejectDialog = page.getByTestId('home-approval-reject-dialog');
  await rejectDialog.waitFor({ state: 'visible', timeout: 8000 });
  const confirmReject = rejectDialog.getByRole('button', { name: '确认驳回' });
  ok(await confirmReject.isDisabled(), '⑤ 驳回意见为空时「确认驳回」禁用');
  await rejectDialog.locator('[data-testid="home-approval-reject-comment"] textarea').fill('不合规');
  await page.waitForTimeout(300);
  ok(await confirmReject.isDisabled(), '⑤ 意见 < 5 字仍禁用（3/5 字）');
  await rejectDialog.locator('[data-testid="home-approval-reject-comment"] textarea').fill('预算不足，退回修改');
  await page.waitForTimeout(300);
  ok(!(await confirmReject.isDisabled()), '⑤ 意见 ≥ 5 字后按钮可用');
  await shots(page, '05-reject-dialog-valid');
  await confirmReject.click();
  await page.locator('.t-message').filter({ hasText: '已驳回' }).first().waitFor({ timeout: 12000 });
  ok(true, '⑤ 驳回成功 Toast「已驳回」');
  await page.waitForTimeout(1500);
  ok((await page.locator('[data-testid^="home-approval-item-"]').filter({ hasText: 'EXP-20261109-0002' }).count()) === 0, '⑤ 驳回后该条消失');
  state = await cardState(page);
  ok(/待办\s*9\s*项/.test(state.stats) && /今日已办\s*3\s*项/.test(state.stats), '⑤ 统计同步：待办 9 / 今日已办 3', state.stats);

  // ---------- ⑥ 加签：选人 → 接口（该条保留） ----------
  const signRow = page.locator('[data-testid^="home-approval-item-"]').filter({ hasText: 'CT-20261108-000002' });
  const signId = await signRow.getAttribute('data-testid');
  await signRow.locator('[data-testid^="home-approval-addsign-"]').click();
  const signDialog = page.getByTestId('home-approval-addsign-dialog');
  await signDialog.waitFor({ state: 'visible', timeout: 8000 });
  const confirmSign = signDialog.getByRole('button', { name: '确认加签' });
  ok(await confirmSign.isDisabled(), '⑥ 未选人时「确认加签」禁用');
  await signDialog.locator('.t-select').first().click();
  await signDialog.locator('.t-select input').first().fill('h5viewer');
  await page.waitForTimeout(1500);
  const option = page.locator('.t-select__dropdown .t-select-option, .t-select__dropdown li').first();
  await option.waitFor({ state: 'visible', timeout: 8000 });
  const optionText = await option.innerText();
  await option.click();
  await page.waitForTimeout(500);
  await shots(page, '06-addsign-dialog');
  await confirmSign.click();
  await page.locator('.t-message').filter({ hasText: '已加签' }).first().waitFor({ timeout: 12000 });
  ok(true, '⑥ 加签成功 Toast', optionText);
  await page.waitForTimeout(1500);
  ok((await page.locator(`[data-testid="${signId}"]`).count()) === 1, '⑥ 加签后该条保留（还要自己批）');
  state = await cardState(page);
  ok(/待办\s*9\s*项/.test(state.stats), '⑥ 加签不改变待办总数', state.stats);
  await shots(page, '07-after-actions');

  ok(consoleErrors.length === 0, '⑩ 超管首页 console error = 0', consoleErrors.slice(0, 3).join(' | '));
  report.details.adminResponses = responses.filter((r) => r.url.includes('/api/business/flow/task')).map((r) => `${r.status} ${r.url.split('?')[0].split('/api')[1]}`);
  await ctx.close();

  // ---------- ⑧⑨ 非超管：卡片可见 + 无待办空态 + 无 403 ----------
  {
    const ctx2 = await browser.newContext({ viewport: { width: 1680, height: 1080 } });
    await ctx2.addInitScript(() => {
      try { localStorage.setItem('cp-onboarding-done', '1'); } catch { /* ignore */ }
    });
    const p2 = await ctx2.newPage();
    const errs = [];
    const resps = [];
    p2.on('console', (m) => { if (m.type() === 'error') errs.push(m.text()); });
    p2.on('pageerror', (e) => errs.push(String(e)));
    p2.on('response', (r) => resps.push({ url: r.url(), status: r.status() }));

    await login(p2, VIEWER);
    ok((await p2.getByTestId('home-approval-card').count()) === 1, '⑨ 非超管也能看到「待我审批」卡');
    const empty = p2.getByTestId('home-approval-empty');
    ok((await empty.count()) === 1, '⑧ 无待办时空态存在');
    ok((await empty.innerText()).includes('没有待我审批的单据'), '⑧ 空态文案 = 没有待我审批的单据', await empty.innerText());
    const s2 = await cardState(p2);
    ok(/待办\s*0\s*项/.test(s2.stats), '⑧ 空态统计为 0', s2.stats);
    await shots(p2, '08-viewer-empty');
    const bad = resps.filter((r) => r.url.includes('/api/') && [401, 403].includes(r.status));
    ok(bad.length === 0, '⑨ 非超管首页无 401/403', bad.map((b) => `${b.status} ${b.url.split('?')[0]}`).join(' | '));
    ok(errs.length === 0, '⑨ 非超管 console error = 0', errs.slice(0, 3).join(' | '));
    await ctx2.close();
  }
} finally {
  await browser.close();
}

console.log(`\n===== ${report.pass.length} passed / ${report.fail.length} failed =====`);
writeFileSync(`${OUT}/report.json`, JSON.stringify(report, null, 2));
process.exit(report.fail.length ? 1 : 0);
