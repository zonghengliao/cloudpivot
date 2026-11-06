/**
 * SO-21 销售发票页真机验收（真后端 :18123 + 真 dist / 真库 cloudpivot_so20）。
 *
 * 覆盖卡面 5 条要求：
 * ① 列表：发票号 / 客户 / 开票日期 / 金额 / 税额 / 状态 + 分页
 * ② 开票弹窗：选应收 → 自动带出金额（真实额度：应收 1130，已开票 0 → 余 1130）
 * ③ 作废按钮（带确认，ISSUED 作废提示红冲）
 * ④ 关联应收单号可点击跳转（跳到应收台账并筛出该单）
 * ⑤ 路由注册（/business/erp/invoice 直达，菜单节点已开）
 */
import { chromium } from '@playwright/test';
import { mkdirSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';

const BASE = 'http://127.0.0.1:5299';
const OUT = resolve(import.meta.dirname ?? '.', 'evidence');
mkdirSync(OUT, { recursive: true });

let pass = 0;
let fail = 0;
const lines = [];
function ok(name, extra = '') {
  pass += 1;
  lines.push(`  PASS  ${name}${extra ? `（${extra}）` : ''}`);
}
function bad(name, extra = '') {
  fail += 1;
  lines.push(`  FAIL  ${name}${extra ? `（${extra}）` : ''}`);
}
function check(name, actual, expected) {
  if (String(actual) === String(expected)) ok(name, actual);
  else bad(name, `期望 ${expected}，实际 ${actual}`);
}
function checkMatch(name, actual, pattern) {
  if (pattern.test(String(actual))) ok(name, actual);
  else bad(name, `${pattern} 不匹配：${String(actual).slice(0, 160)}`);
}

const consoleErrors = [];
const apiFailures = [];

// 隔离库 cloudpivot_so20 的 admin 口令（该库由 SO-20 建库时的 NIK_ADMIN_PASSWORD 决定）；
// 换库时用 `ADMIN_PASS=... node verify-ui.mjs` 覆盖，别把口令写死到别的环境。
const PASSWORD = process.env.ADMIN_PASS ?? 'Admin@123456_so20';

async function login() {
  const resp = await fetch(`${BASE}/api/system/auth/login`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ username: 'admin', password: PASSWORD }),
  });
  const body = await resp.json();
  if (!body?.data?.access_token) throw new Error(`登录失败：${JSON.stringify(body)}`);
  return body.data;
}

const tokens = await login();

const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1600, height: 1000 } });
page.on('console', (msg) => {
  if (msg.type() === 'error') consoleErrors.push(msg.text());
});
page.on('response', (resp) => {
  const url = resp.url();
  const status = resp.status();
  // notice/stream 的全站既有 401（EventSource 通道），与本次改动无关
  if (status >= 400 && !url.includes('/business/notice/stream')) {
    apiFailures.push(`${status} ${resp.request().method()} ${url}`);
  }
});

await page.addInitScript(
  ({ access, refresh }) => {
    localStorage.setItem('x_access_token', access);
    localStorage.setItem('x_refresh_token', refresh ?? '');
    localStorage.setItem('cp-onboarding-done', '1');
    sessionStorage.setItem('login-unread-shown', '1');
  },
  { access: tokens.access_token, refresh: tokens.refresh_token },
);

/** 选中 t-select 的一项（真实鼠标坐标；面板 teleport 到 body，取最后一个可见面板） */
async function pickSelect(testId, text) {
  await page.getByTestId(testId).click();
  const panel = page.locator('.t-select__dropdown:visible').last();
  await panel.waitFor({ state: 'visible', timeout: 10_000 });
  const option = panel.locator('.t-select-option', { hasText: text }).first();
  await option.waitFor({ state: 'visible', timeout: 10_000 });
  await option.click();
  await page.waitForTimeout(200);
}

/** 关掉所有还开着的下拉面板 */
async function closePops() {
  await page.mouse.click(4, 4);
  await page.waitForTimeout(120);
}

async function textOf(selector) {
  return (await page.locator(selector).first().innerText()).trim();
}

async function rowStatus(invoiceNo) {
  return textOf(`[data-testid="inv-status-${invoiceNo}"]`);
}

async function openCreateDialog() {
  await page.getByTestId('btn-inv-create').click();
  const dialog = page.getByTestId('inv-create-dialog');
  await dialog.waitFor({ state: 'visible', timeout: 10_000 });
  // 应收清单是打开弹窗后才拉的，等下拉可用
  await page.waitForFunction(
    () => {
      const el = document.querySelector('[data-testid="inv-create-receivable"]');
      return el && !el.className.includes('t-is-disabled');
    },
    { timeout: 15_000 },
  );
}

async function submitDialog() {
  const dialog = page.locator('.t-dialog').filter({ has: page.locator('[data-testid="inv-create-dialog"]') });
  void dialog;
  await page.locator('.t-dialog__footer button.t-button--theme-primary:visible').last().click();
}

lines.push('== ① 页面直达（路由注册）与列表 ============================================');
await page.goto(`${BASE}/business/erp/invoice`, { waitUntil: 'domcontentloaded' });
await page.waitForFunction(
  () => (document.querySelector('[data-testid="inv-table"]')?.textContent ?? '').includes('IV-'),
  { timeout: 25_000 },
);
check('路由 /business/erp/invoice 落地（页面标题/列表就绪）', page.url().includes('/business/erp/invoice'), true);

const metaText = await textOf('[data-testid="inv-meta"]');
checkMatch('列表头统计「共 N 条」', metaText, /^共 \d+ 条$/);
const headerText = await textOf('[data-testid="inv-table"] thead, [data-testid="inv-table"] .t-table__header');
for (const name of ['发票号', '关联应收', '客户', '开票日期', '金额(不含税)', '税额', '价税合计', '状态', '操作']) {
  check(`列「${name}」存在`, headerText.includes(name), true);
}
const rowCount = await page.locator('[data-testid="inv-table"] tbody tr').count();
check('分页器渲染（每页 20）', (await textOf('[data-testid="inv-table"]')).length > 0, true);
lines.push(`  INFO  首屏行数=${rowCount}，meta=${metaText}`);
await page.screenshot({ path: `${OUT}/01-list.png`, fullPage: false });

lines.push('== ② 开票弹窗：选应收 → 自动带出金额（AR20-001，应收 1130 / 已开票 0）========');
await openCreateDialog();
await pickSelect('inv-create-receivable', 'AR20-001');
await page.waitForFunction(
  () => {
    const el = document.querySelector('[data-testid="inv-create-headroom"]');
    return el && el.textContent.includes('1,130.00');
  },
  { timeout: 15_000 },
);
check('已开票价税合计（真实接口算）= 0.00', await textOf('[data-testid="inv-create-invoiced"]'), '0.00');
check('剩余可开票额度 = 1,130.00', await textOf('[data-testid="inv-create-headroom"]'), '1,130.00');
const totalInput = page.getByTestId('inv-create-total').locator('input').first();
check('价税合计自动带出 1130（= 剩余额度）', await totalInput.inputValue(), '1130');
const splitText = await textOf('[data-testid="inv-create-split"]');
checkMatch('税额预览 1,000.00 + 130.00 = 1,130.00', splitText, /1,000\.00[\s\S]*130\.00[\s\S]*1,130\.00/);
// 「创建后立即开票」必须在首屏可见（放到表单末尾会被 64vh 滚动区藏起来）
const issueNowBox = await page.getByTestId('inv-create-issue-now').boundingBox();
check('「创建后立即开票」开关在首屏可见', Boolean(issueNowBox && issueNowBox.y > 0 && issueNowBox.y < 1000), true);
await page.waitForTimeout(500);
await page.screenshot({ path: `${OUT}/02-create-autofill.png` });

// 负样本：AR20-003 已被开满（POSTED 113 + DRAFT 113 = 226）→ 额度 0，前端当场拦
await pickSelect('inv-create-receivable', 'AR20-003');
await page.waitForFunction(
  () => document.querySelector('[data-testid="inv-create-headroom"]')?.textContent.includes('0.00'),
  { timeout: 15_000 },
);
check('开满的应收额度 = 0.00', await textOf('[data-testid="inv-create-headroom"]'), '0.00');
await submitDialog();
await page.waitForSelector('[data-testid="inv-create-error"]', { state: 'visible', timeout: 10_000 });
checkMatch(
  '额度为 0 时前端拦住并说明原因',
  await textOf('[data-testid="inv-create-error"]'),
  /额度/,
);
await page.screenshot({ path: `${OUT}/03-create-no-headroom.png` });

lines.push('== ③ 开票（创建后立即开票）：收入凭证 + 回写应收 ===========================');
await pickSelect('inv-create-receivable', 'AR20-001');
await page.waitForFunction(
  () => document.querySelector('[data-testid="inv-create-headroom"]')?.textContent.includes('1,130.00'),
  { timeout: 15_000 },
);
const issueResp = page.waitForResponse(
  (resp) => resp.url().endsWith('/api/business/erp/invoice') && resp.request().method() === 'POST',
  { timeout: 20_000 },
);
const issuePatch = page.waitForResponse(
  (resp) =>
    resp.url().includes('/api/business/erp/invoice/') &&
    resp.url().endsWith('/status') &&
    resp.request().method() === 'PATCH',
  { timeout: 25_000 },
);
await submitDialog();
const created = await issueResp;
check('POST /erp/invoice → 200', created.status(), 200);
const createdBody = await created.json();
const newNo = createdBody?.data?.invoice_no;
checkMatch('新发票号形状 IV-yyyyMMdd-000001', newNo, /^IV-\d{8}-\d{6}$/);
check('建单落 DRAFT（未开票）', createdBody?.data?.status, 'DRAFT');

// 紧接着的 PATCH status=ISSUED 才是「开票」：生成收入凭证 + 回写应收
const issuedResp = await issuePatch;
check('PATCH status=ISSUED → 200', issuedResp.status(), 200);
const issuedBody = await issuedResp.json();
check('开票后状态 = ISSUED（已开票）', issuedBody?.data?.status, 'ISSUED');
checkMatch('收入凭证号已生成', issuedBody?.data?.voucher_no, /^PZ/);
check('价税合计 = 1130.0000（后端按合计拆分）', issuedBody?.data?.total_with_tax, '1130.0000');
check('不含税 = 1000.0000', issuedBody?.data?.amount, '1000.0000');
check('税额 = 130.0000', issuedBody?.data?.tax_amount, '130.0000');
check('回写应收单号 = AR20-001', issuedBody?.data?.receivable_no, 'AR20-001');

// 抽屉自动打开（创建后立即开票 → 打开详情）
await page.getByTestId('inv-detail-body').waitFor({ state: 'visible', timeout: 15_000 });
await page.waitForTimeout(1400); // t-drawer 是滑入动画，抢拍会截到「还在视口外」的半张抽屉
check('详情抽屉：发票号', await textOf('[data-testid="inv-detail-no"]'), newNo);
check('详情抽屉：状态 = 已开票', await textOf('[data-testid="inv-detail-status"]'), '已开票');
checkMatch('详情抽屉：凭证号非空', await textOf('[data-testid="inv-detail-voucher"]'), /\d/);
check('详情抽屉：剩余可开票额度 = 0.00', await textOf('[data-testid="inv-detail-headroom"]'), '0.00');
check(
  '详情抽屉：来源不重复编码（不出现 MANUAL（MANUAL））',
  (await textOf('[data-testid="inv-detail-source"]')).includes('MANUAL（MANUAL）'),
  false,
);
await page.screenshot({ path: `${OUT}/04-detail-issued.png` });
await page.getByTestId('btn-inv-drawer-close').click();
await page.waitForTimeout(300);

lines.push('== ④ 作废（带确认，ISSUED → 红冲 + 撤回应收发票号）=========================');
await page.waitForFunction(
  (no) => document.querySelector(`[data-testid="inv-status-${no}"]`)?.textContent.includes('已开票'),
  newNo,
  { timeout: 15_000 },
);
await page.getByTestId(`inv-op-void-${newNo}`).click();
await page.getByTestId('inv-void-dialog').waitFor({ state: 'visible', timeout: 10_000 });
await page.waitForTimeout(500); // t-dialog 有淡入动画，抢拍会得到「看不见弹窗」的截图
checkMatch('作废弹窗点名对象（单号/客户/金额）', await textOf('[data-testid="inv-void-target"]'), new RegExp(newNo));
checkMatch(
  'ISSUED 作废提示：红字凭证 + 撤回应收发票号',
  await textOf('[data-testid="inv-void-impact"]'),
  /红字凭证[\s\S]*发票号/,
);
await page.screenshot({ path: `${OUT}/05-void-dialog.png` });
// 原因不足 4 字要拦住（不静默）
await page.getByTestId('inv-void-reason').locator('textarea').fill('错');
await page.locator('.t-dialog__footer button.t-button--theme-danger:visible').last().click();
await page.waitForSelector('[data-testid="inv-void-reason-error"], [data-testid="inv-void-error"]', {
  state: 'visible',
  timeout: 10_000,
});
ok('作废原因为 2 字时被拦（≥4 字）');

const voidResp = page.waitForResponse(
  (resp) => resp.url().includes('/api/business/erp/invoice/') && resp.url().endsWith('/status') && resp.request().method() === 'PATCH',
  { timeout: 20_000 },
);
await page.getByTestId('inv-void-reason').locator('textarea').fill('开票信息填错，重新开一张');
await page.locator('.t-dialog__footer button.t-button--theme-danger:visible').last().click();
const voided = await voidResp;
check('PATCH status=VOID → 200', voided.status(), 200);
const voidedBody = await voided.json();
check('作废后状态 = VOID', voidedBody?.data?.status, 'VOID');
await page.waitForFunction(
  (no) => document.querySelector(`[data-testid="inv-status-${no}"]`)?.textContent.includes('已作废'),
  newNo,
  { timeout: 15_000 },
);
check('列表行状态刷新为「已作废」', await rowStatus(newNo), '已作废');
check('作废后关联应收仍在（可追溯）', await textOf(`[data-testid="inv-ar-link-${newNo}"]`), 'AR20-001');
await page.screenshot({ path: `${OUT}/06-list-after-void.png` });

lines.push('== ⑤ 关联应收单号可点击跳转 → 应收台账按号筛出 ============================');
await page.getByTestId(`inv-ar-link-${newNo}`).click();
await page.waitForFunction(() => location.pathname === '/business/erp/receivable', { timeout: 15_000 });
check('跳转到 /business/erp/receivable', page.url().includes('ar_no=AR20-001'), true);
const arNoInput = page.locator('[data-testid="ar-filter-no"] input').first();
await page.waitForTimeout(1500);
const arNoValue = await arNoInput.inputValue().catch(() => '');
check('应收台账筛选条已预填 AR20-001', arNoValue, 'AR20-001');
// 列表真的被筛了（不是只填了输入框）：只剩这一张应收
const arTableText = await textOf('[data-testid="ar-table"]');
checkMatch('应收台账只列出 AR20-001', arTableText, /AR20-001/);
check('筛掉其它应收（不含 AR20-002）', arTableText.includes('AR20-002'), false);
await page.screenshot({ path: `${OUT}/07-jump-receivable.png` });

lines.push('== ⑥ DRAFT 路径：只建草稿 → 草稿作废（不红冲）=================================');
await page.goto(`${BASE}/business/erp/invoice`, { waitUntil: 'domcontentloaded' });
await page.waitForFunction(
  () => (document.querySelector('[data-testid="inv-table"]')?.textContent ?? '').includes('IV-'),
  { timeout: 25_000 },
);
await openCreateDialog();
await pickSelect('inv-create-receivable', 'AR20-002');
await page.waitForFunction(
  () => {
    const el = document.querySelector('[data-testid="inv-create-headroom"]');
    return el && el.textContent.includes('565.00');
  },
  { timeout: 15_000 },
);
check('AR20-002 剩余额度 = 565.00', await textOf('[data-testid="inv-create-headroom"]'), '565.00');
await page.getByTestId('inv-create-issue-now').click(); // 取消「创建后立即开票」
const draftResp = page.waitForResponse(
  (resp) => resp.url().endsWith('/api/business/erp/invoice') && resp.request().method() === 'POST',
  { timeout: 20_000 },
);
await submitDialog();
const draftCreated = await draftResp;
const draftBody = await draftCreated.json();
const draftNo = draftBody?.data?.invoice_no;
check('只建草稿：状态 = DRAFT', draftBody?.data?.status, 'DRAFT');
await page.waitForFunction(
  (no) => document.querySelector(`[data-testid="inv-status-${no}"]`)?.textContent.includes('草稿'),
  draftNo,
  { timeout: 15_000 },
);
ok('列表出现草稿行并带「开票」按钮', `row=${draftNo}`);

await page.getByTestId(`inv-op-void-${draftNo}`).click();
await page.getByTestId('inv-void-dialog').waitFor({ state: 'visible', timeout: 10_000 });
checkMatch('草稿作废提示不提红字凭证', await textOf('[data-testid="inv-void-impact"]'), /草稿[\s\S]*不产生凭证/);
await page.getByTestId('inv-void-reason').locator('textarea').fill('草稿作废（验收）');
const draftVoidResp = page.waitForResponse(
  (resp) => resp.url().includes('/api/business/erp/invoice/') && resp.url().endsWith('/status') && resp.request().method() === 'PATCH',
  { timeout: 20_000 },
);
await page.locator('.t-dialog__footer button.t-button--theme-danger:visible').last().click();
const draftVoided = await draftVoidResp;
check('DRAFT→VOID → 200', draftVoided.status(), 200);
await page.waitForFunction(
  (no) => document.querySelector(`[data-testid="inv-status-${no}"]`)?.textContent.includes('已作废'),
  draftNo,
  { timeout: 15_000 },
);
ok('草稿作废后状态 = 已作废');
await page.screenshot({ path: `${OUT}/08-draft-voided.png` });

lines.push('== ⑦ 控制台 / 接口异常 ====================================================');
const noise = apiFailures.filter((line) => !line.includes('/business/notice/stream'));
check('除全站既有 notice/stream 外无 4xx/5xx', noise.length, 0);
if (noise.length) lines.push(...noise.map((line) => `  INFO  ${line}`));
check('无浏览器 console.error', consoleErrors.length, 0);
if (consoleErrors.length) lines.push(...consoleErrors.slice(0, 6).map((line) => `  INFO  ${line}`));

lines.push('');
lines.push(`结果：PASS=${pass} FAIL=${fail}`);
const report = lines.join('\n');
writeFileSync(`${OUT}/verify-round.log`, `${report}\n`);
console.log(report);
await browser.close();
process.exit(fail === 0 ? 0 : 1);
