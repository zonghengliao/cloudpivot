/**
 * A5 合同管理页真机验收（Playwright，无人工介入）。
 *
 * 覆盖卡面验收 13–20：
 *   13 登录 → 合同管理 → 列表打开，不 404
 *   14 新建一张合同（选乙方、填金额、选日期）→ 保存成功 → 列表能看到
 *   15 登记签订 → 生效 → 状态 ACTIVE，徽标变绿
 *   16 终止（填原因）→ TERMINATED
 *   17 对 DRAFT 单点删除 → 成功
 *   18 对已生效单不显示删除按钮
 *   19 控制台无报错、无 4xx/5xx
 *   20 截图 4 张：列表 / 新建表单 / 生效后 / 详情抽屉
 *
 * 跑法（node_modules 从 /opt/cloudpivot/frontend 软链过来）：
 *   node verify.mjs http://127.0.0.1:5215 <evidence-dir>
 */
import { chromium } from '@playwright/test';
import { mkdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const BASE = process.argv[2] ?? 'http://127.0.0.1:5215';
const EVIDENCE = process.argv[3] ?? './evidence';
mkdirSync(EVIDENCE, { recursive: true });

const USER = 'admin';
const PASSWORD = 'A5@Verify2026';
const STAMP = Date.now().toString().slice(-6);
const NAME = `A5 真机验收合同-${STAMP}`;
const NAME_TO_DELETE = `A5 待删草稿-${STAMP}`;

const checks = [];
const consoleErrors = [];
const badResponses = [];
const pageErrors = [];

function check(name, ok, detail = '') {
  checks.push({ name, ok: Boolean(ok), detail });
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? `  — ${detail}` : ''}`);
}

const browser = await chromium.launch({ args: ['--no-sandbox'] });
const context = await browser.newContext({ viewport: { width: 1600, height: 1000 }, locale: 'zh-CN' });
// 首次进站的「新功能引导」蒙层会拦截所有点击（踩过）：开局标记已完成
await context.addInitScript(() => {
  window.localStorage.setItem('cp-onboarding-done', '1');
});
const page = await context.newPage();

page.on('console', (msg) => {
  if (msg.type() === 'error') {
    consoleErrors.push(msg.text());
  }
});
page.on('pageerror', (error) => pageErrors.push(String(error)));
page.on('response', (response) => {
  if (response.status() >= 400) {
    badResponses.push(`${response.status()} ${response.request().method()} ${response.url()}`);
  }
});

/** 等某接口的响应（method + 路径片段） */
function waitApi(method, fragment) {
  return page.waitForResponse(
    (response) => response.request().method() === method && response.url().includes(fragment),
    { timeout: 20000 },
  );
}

/** 选中 `t-select` 里的一个选项（下拉 teleport 到 body，取最后一个可见面板） */
async function pickOption(selectLocator, optionText) {
  // `data-testid` 落在 `<t-select>` 上时就是 `.t-select__wrap` 本身（实测），
  // 内置了 `.t-select__wrap` 的写法要兼容一下
  const wrap = (await selectLocator.locator('.t-select__wrap').count()) > 0
    ? selectLocator.locator('.t-select__wrap').first()
    : selectLocator;
  await wrap.click();
  await page.waitForTimeout(300);
  const dropdowns = page.locator('.t-select__dropdown:visible');
  const count = await dropdowns.count();
  const dropdown = dropdowns.nth(count - 1);
  const option = dropdown.locator('.t-select-option', { hasText: optionText }).first();
  await option.click();
  await page.waitForTimeout(250);
}

/** 填 t-input-number / t-input 的真实 input（受控组件，fill 会被回写打断） */
async function typeInto(locator, value) {
  const input = locator.locator('input').first();
  await input.click();
  await input.press('Control+a');
  await input.press('Backspace');
  await input.type(String(value), { delay: 20 });
  await input.press('Tab');
}

// ---------------------------------------------------------------- 13 登录 + 列表
await page.goto(`${BASE}/login`, { waitUntil: 'networkidle' });
await page.getByPlaceholder('请输入用户名').fill(USER);
await page.getByPlaceholder('请输入密码').fill(PASSWORD);
await page.locator('button[type="submit"]').click();
await page.waitForURL((url) => !url.pathname.includes('/login'), { timeout: 20000 });
check('13a 登录成功（离开 /login）', true, page.url());

await page.goto(`${BASE}/business/erp/contract`, { waitUntil: 'networkidle' });
await page.waitForSelector('[data-testid="contract-table"]', { timeout: 20000 });
const titleText = await page.title();
check('13b 合同管理列表打开（非 404 页）', !titleText.includes('页面不存在'), `title=${titleText}`);
const hasFilters = await page.locator('[data-testid="contract-filter-no"]').isVisible();
check('13c 筛选区渲染', hasFilters);
const emptyVisible = await page.locator('[data-testid="contract-empty"]').isVisible().catch(() => false);
check('13d 空库时给空态提示（不空白）', emptyVisible);
await page.screenshot({ path: join(EVIDENCE, '01-list-empty.png'), fullPage: true });

// ---------------------------------------------------------------- 14 新建合同
await page.locator('[data-testid="btn-contract-create"]').click();
await page.waitForSelector('[data-testid="contract-form-dialog"] .t-dialog', { timeout: 10000 });
await page.waitForTimeout(300);

await page.locator('[data-testid="contract-form-name"] input').fill(NAME);
await typeInto(page.locator('[data-testid="contract-form-amount"]'), '888888.88');
// 乙方：唯一一个种子供应商
await pickOption(page.locator('[data-testid="contract-form-party-b"]'), '示例供应商');
await page.screenshot({ path: join(EVIDENCE, '02-form-create.png'), fullPage: true });

const createResponse = waitApi('POST', '/api/business/erp/contract');
await page.locator('[data-testid="btn-contract-save"]').click();
const created = await createResponse;
check('14a 新建合同接口 200', created.status() === 200, `status=${created.status()}`);
const createdBody = await created.json();
const contractNo = createdBody?.data?.contract_no ?? '';
check('14b 合同号自动生成（HT-yyyyMMdd-000001）', /^HT-\d{8}-\d{6}$/.test(contractNo), contractNo);
await page.waitForSelector(`[data-testid="contract-no-${contractNo}"]`, { timeout: 15000 });
check('14c 保存后列表能看到这张合同', true, contractNo);

const amountText = await page.locator(`[data-testid="contract-amount-${contractNo}"]`).innerText();
check('14d 金额列千分位 + ¥ 前缀', amountText.trim() === '¥888,888.88', amountText.trim());
// 截图前等动画落定 + toast 消失（`t-dialog` 关闭只做 display 切换，message 默认 3s）
await page.waitForTimeout(3400);
await page.screenshot({ path: join(EVIDENCE, '05-list-with-rows.png'), fullPage: true });

// ---------------------------------------------------------------- 15 登记签订 → 生效
const signResponse = waitApi('PATCH', `/api/business/erp/contract/`);
await page.locator(`[data-testid="btn-contract-to-SIGNED-${contractNo}"]`).click();
await page.waitForSelector('[data-testid="contract-status-dialog"] .t-dialog', { timeout: 10000 });
await page.locator('[data-testid="contract-status-dialog"] .t-dialog__footer button.t-button--theme-primary').click();
const signed = await signResponse;
check('15a 登记签订接口 200', signed.status() === 200, `status=${signed.status()}`);

await page.waitForSelector(`[data-testid="btn-contract-to-ACTIVE-${contractNo}"]`, { timeout: 15000 });
const activeResponse = waitApi('PATCH', '/api/business/erp/contract/');
await page.locator(`[data-testid="btn-contract-to-ACTIVE-${contractNo}"]`).click();
await page.waitForSelector('[data-testid="contract-status-dialog"] .t-dialog', { timeout: 10000 });
await page.locator('[data-testid="contract-status-dialog"] .t-dialog__footer button.t-button--theme-primary').click();
const activated = await activeResponse;
check('15b 生效接口 200', activated.status() === 200);

await page.waitForFunction(
  (id) => (document.querySelector(`[data-testid="${id}"]`)?.getAttribute('class') ?? '').includes('t-tag--success'),
  `contract-status-${contractNo}`,
  { timeout: 15000 },
);
const activeBadge = await page.locator(`[data-testid="contract-status-${contractNo}"]`).innerText();
check('15c 状态徽标显示「已生效」', activeBadge.includes('已生效'), activeBadge.trim());
const activeTagClass = await page.locator(`[data-testid="contract-status-${contractNo}"]`).getAttribute('class');
check('15d 徽标配色为 success（绿）', (activeTagClass ?? '').includes('t-tag--success'), activeTagClass ?? '');
await page.waitForTimeout(3400);
await page.screenshot({ path: join(EVIDENCE, '03-status-active.png'), fullPage: true });

// ---------------------------------------------------------------- 18 已生效单没有删除按钮
const removeOnActive = await page.locator(`[data-testid="btn-contract-remove-${contractNo}"]`).count();
check('18 已生效单不显示删除按钮', removeOnActive === 0, `count=${removeOnActive}`);
const editOnActive = await page.locator(`[data-testid="btn-contract-edit-${contractNo}"]`).count();
check('11b 已生效单不显示编辑按钮', editOnActive === 0, `count=${editOnActive}`);

// ---------------------------------------------------------------- 16 终止（必填原因）
const terminateResponse = waitApi('PATCH', '/api/business/erp/contract/');
await page.locator(`[data-testid="btn-contract-to-TERMINATED-${contractNo}"]`).click();
await page.waitForSelector('[data-testid="contract-status-reason"] textarea', { timeout: 10000 });
await page.locator('[data-testid="contract-status-reason"] textarea').fill('A5 真机验收：模拟供应商违约终止');
await page.locator('[data-testid="contract-status-dialog"] .t-dialog__footer button.t-button--theme-primary').click();
const terminated = await terminateResponse;
check('16a 终止接口 200', terminated.status() === 200);
await page.waitForFunction(
  (id) => (document.querySelector(`[data-testid="${id}"]`)?.getAttribute('class') ?? '').includes('t-tag--warning'),
  `contract-status-${contractNo}`,
  { timeout: 15000 },
);
const terminatedBadge = await page.locator(`[data-testid="contract-status-${contractNo}"]`).innerText();
check('16b 状态变为「已终止」且徽标橙色（warning）', terminatedBadge.includes('已终止'), terminatedBadge.trim());
await page.waitForTimeout(3400);
await page.screenshot({ path: join(EVIDENCE, '06-status-terminated.png'), fullPage: true });

// ---------------------------------------------------------------- 20 详情抽屉
await page.locator(`[data-testid="btn-contract-view-${contractNo}"]`).click();
await page.waitForSelector('[data-testid="contract-detail"]', { timeout: 15000 });
await page.waitForTimeout(600);
const detailName = await page.locator('[data-testid="contract-detail-name"]').innerText();
check('20a 详情抽屉打开且字段铺开', detailName.trim() === NAME, detailName.trim());
const detailAmount = await page.locator('[data-testid="contract-detail-amount"]').innerText();
check('20b 详情金额同样千分位 + ¥', detailAmount.trim() === '¥888,888.88', detailAmount.trim());
const linkedEmpty = await page.locator('[data-testid="contract-detail-linked-empty"]').isVisible().catch(() => false);
const linkedTable = await page.locator('[data-testid="contract-detail-linked"]').isVisible().catch(() => false);
check('20c 关联采购单区块有内容（列表或空态说明）', linkedEmpty || linkedTable, linkedEmpty ? '空态说明' : '有采购单');
const detailActionsEmpty = await page.locator('[data-testid="contract-detail-actions-empty"]').isVisible().catch(() => false);
check('7b 终态在详情里也没有流转按钮', detailActionsEmpty);
await page.screenshot({ path: join(EVIDENCE, '04-detail-drawer.png'), fullPage: true });
await page.locator('[data-testid="btn-contract-detail-close"]').click();
await page.waitForTimeout(400);

// ---------------------------------------------------------------- 17 删除 DRAFT
await page.locator('[data-testid="btn-contract-create"]').click();
await page.waitForSelector('[data-testid="contract-form-dialog"] .t-dialog', { timeout: 10000 });
await page.locator('[data-testid="contract-form-name"] input').fill(NAME_TO_DELETE);
await typeInto(page.locator('[data-testid="contract-form-amount"]'), '1000');
await pickOption(page.locator('[data-testid="contract-form-party-b"]'), '示例供应商');
const createDraft = waitApi('POST', '/api/business/erp/contract');
await page.locator('[data-testid="btn-contract-save"]').click();
const draftBody = await (await createDraft).json();
const draftNo = draftBody?.data?.contract_no ?? '';
await page.waitForSelector(`[data-testid="btn-contract-remove-${draftNo}"]`, { timeout: 15000 });
check('17a 草稿单显示删除入口', true, draftNo);

const deleteResponse = waitApi('DELETE', '/api/business/erp/contract/');
await page.locator(`[data-testid="btn-contract-remove-${draftNo}"]`).click();
// DialogPlugin 的确认弹窗没有 `.t-dialog__body`，且关掉的弹窗仍留在 DOM ——
// 直接按按钮文案点，别用 `.t-dialog:visible` 这类位置选择器
const confirmDelete = page.getByRole('button', { name: '确认删除', exact: true });
await confirmDelete.waitFor({ state: 'visible', timeout: 10000 });
await confirmDelete.click({ noWaitAfter: true }).catch(() => undefined);
const deleted = await deleteResponse;
check('17b 删除接口 200', deleted.status() === 200, `status=${deleted.status()}`);
await page.waitForFunction(
  (no) => !document.body.innerText.includes(no),
  draftNo,
  { timeout: 15000 },
);
check('17c 删除后列表不再有这张合同', true);

// 刷新后再确认一次（防止只是前端本地移除）
await page.reload({ waitUntil: 'networkidle' });
await page.waitForSelector('[data-testid="contract-table"]', { timeout: 20000 });
const stillThere = await page.locator('body').innerText();
check('17d 刷新后依然不在（软删生效）', !stillThere.includes(draftNo), draftNo);

// ---------------------------------------------------------------- 19 控制台 / 网络
// 全局已知缺陷（**与本卡无关**）：`/api/business/notice/stream` 的 SSE 用 `?token=` 鉴权，
// 而 alun 0.4.2 的认证层没有查询令牌白名单（`NIK_AUTH_QUERY_TOKEN_PATHS` 未实现），
// 所以每个页面（含 /dashboard、/supplier）都会挂一条 401。用 `probe-sse.mjs` 单独取证，
// 这里只断言「合同页自身的请求与控制台是干净的」。
const SSE_PATH = '/api/business/notice/stream';
const ownConsoleErrors = consoleErrors.filter((text) => !text.includes('401'));
const ownBadResponses = badResponses.filter((entry) => !entry.includes(SSE_PATH));
const sseBadResponses = badResponses.filter((entry) => entry.includes(SSE_PATH));

check('19a 无页面级 JS 异常', pageErrors.length === 0, pageErrors.join(' | '));
check('19b 无 console.error（除全局 SSE 401）', ownConsoleErrors.length === 0, ownConsoleErrors.slice(0, 3).join(' | '));
check('19c 合同页自身请求无 4xx/5xx（除全局 SSE 401）', ownBadResponses.length === 0, ownBadResponses.slice(0, 5).join(' | '));
check(
  '19d 剩余 4xx 只有全局 SSE 401（既有缺陷，见 evidence/sse-401-global.txt）',
  badResponses.every((entry) => entry.includes(SSE_PATH)),
  `SSE 401 × ${sseBadResponses.length}`,
);

const failed = checks.filter((item) => !item.ok);
writeFileSync(join(EVIDENCE, 'checks.json'), JSON.stringify({ checks, consoleErrors, pageErrors, badResponses }, null, 2));
console.log(`\n=== ${checks.length - failed.length}/${checks.length} PASS ===`);
if (failed.length > 0) {
  console.log('FAILED:', failed.map((item) => item.name).join(', '));
}

await browser.close();
process.exit(failed.length === 0 ? 0 : 1);
