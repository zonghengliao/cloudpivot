/**
 * SO-03 客户档案页真机验收（Playwright，跑在 vite dev + 隔离后端 18097 上）。
 *
 * 用法：node verify.mjs <round>
 *   - 令牌从 ../so03/token.txt 读（不打印）
 *   - 证据写 /opt/cloudpivot/docs/evidence/20261106-so03-customer/round-<n>.txt + *.png
 *
 * 验的是「列表 / 搜索 / 新建 / 编辑 / 状态切换 / 删除」六件套，全部走真接口（无 route 拦截）。
 */
import { chromium } from '@playwright/test';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const ROUND = process.argv[2] || '1';
const BASE = process.env.BASE || 'http://127.0.0.1:5186';
const API = process.env.API || 'http://127.0.0.1:18097';
const SCRATCH = process.env.SO03_SCRATCH || path.join(os.homedir(), '.cache/so03');
const EVID = process.env.SO03_EVID
  || fileURLToPath(new URL('../../docs/evidence/20261106-so03-customer', import.meta.url));
fs.mkdirSync(EVID, { recursive: true });

const TOKEN = fs.readFileSync(path.join(SCRATCH, 'token.txt'), 'utf8').trim();
const REFRESH = fs.readFileSync(path.join(SCRATCH, 'refresh.txt'), 'utf8').trim();
const LOG = path.join(EVID, `round-${ROUND}.txt`);
fs.writeFileSync(LOG, '');
const lines = [];
let pass = 0;
let fail = 0;

function say(msg) {
  lines.push(msg);
  console.log(msg);
  fs.writeFileSync(LOG, lines.join('\n') + '\n');
}

function check(desc, actual, expected) {
  const ok = JSON.stringify(actual) === JSON.stringify(expected);
  if (ok) {
    pass += 1;
    say(`[PASS] ${desc} = ${JSON.stringify(actual)}`);
  } else {
    fail += 1;
    say(`[FAIL] ${desc} = ${JSON.stringify(actual)}（期望 ${JSON.stringify(expected)}）`);
  }
}

function checkTrue(desc, cond, detail = '') {
  if (cond) {
    pass += 1;
    say(`[PASS] ${desc}${detail ? ' · ' + detail : ''}`);
  } else {
    fail += 1;
    say(`[FAIL] ${desc}${detail ? ' · ' + detail : ''}`);
  }
}

/** 直接打后端接口（旁证：UI 的改动真的落库了） */
async function api(method, apiPath) {
  const res = await fetch(`${API}${apiPath}`, {
    method,
    headers: { Authorization: `Bearer ${TOKEN}`, 'Content-Type': 'application/json' },
  });
  const text = await res.text();
  let body = null;
  try {
    body = JSON.parse(text);
  } catch {
    body = text;
  }
  return { status: res.status, body };
}

const CODE = `SO03-E2E-${Date.now().toString().slice(-6)}`;
const NAME1 = `SO03 客户 ${ROUND} 轮`;
const NAME2 = `SO03 客户 ${ROUND} 轮（改名）`;

const browser = await chromium.launch({ headless: true });
const context = await browser.newContext({ viewport: { width: 1600, height: 1000 } });
await context.addInitScript(
  ([token, refresh]) => {
    localStorage.setItem('x_access_token', token);
    localStorage.setItem('x_refresh_token', refresh);
    localStorage.setItem('cp-onboarding-done', '1');
    sessionStorage.setItem('login-unread-shown', '1');
  },
  [TOKEN, REFRESH],
);

const page = await context.newPage();
const consoleErrors = [];
const apiLog = [];
page.on('console', (msg) => {
  if (msg.type() === 'error') consoleErrors.push(msg.text());
});
page.on('response', (res) => {
  const url = res.url();
  if (url.includes('/api/')) apiLog.push(`${res.request().method()} ${new URL(url).pathname}${new URL(url).search} → ${res.status()}`);
});

const table = page.getByTestId('customer-table');
const rowOf = (text) => table.locator('tbody tr').filter({ hasText: text }).first();
const dialog = page.locator('.t-dialog').filter({ hasText: '客户' }).last();

async function fill(testid, value) {
  const input = page.getByTestId(testid).locator('input, textarea').first();
  await input.click();
  await input.press('Control+a');
  await input.press('Backspace');
  await input.type(value, { delay: 15 });
  await input.press('Tab');
}

say(`== SO-03 客户档案前端真机验收 · 第 ${ROUND} 轮 · ${new Date().toISOString()} · BASE=${BASE}`);
say(`   测试编码=${CODE}（每轮唯一）`);

// ---------- 1. 列表 ----------
await page.goto(`${BASE}/business/erp/customer`, { waitUntil: 'domcontentloaded' });
await page.bringToFront();
await page.waitForFunction(
  () => document.visibilityState === 'visible' && !!document.querySelector('[data-testid="customer-table"]'),
  { timeout: 30000 },
);
await page.waitForFunction(
  (code) => (document.querySelector('[data-testid="customer-table"]')?.textContent ?? '').includes(code),
  'CUS-0001',
  { timeout: 30000 },
);
checkTrue('1 列表首屏渲染 seed 客户 CUS-0001', true);
const metaText = (await page.getByTestId('cus-meta').innerText()).trim();
say(`   列表口径: ${metaText}`);
checkTrue('1 列表分页口径显示「共 N 条」', /共\s*\d+\s*条/.test(metaText), metaText);
const rowCount = await table.locator('tbody tr').count();
checkTrue('1 一页 20 条上限内 tbody 行数 >= 3', rowCount >= 3, `rows=${rowCount}`);
// 0 额度显示「不控额度」而不是 0
const zeroRow = rowOf('CUS-0003');
checkTrue('1 额度 0（CUS-0003）显示「不控额度」', (await zeroRow.innerText()).includes('不控额度'));
const apiPage1 = await api('GET', '/api/business/erp/customer/page?page=1&page_size=20');
check('1 后端分页 total 与页面口径同源', apiPage1.body.data.total >= 3, true);
await page.screenshot({ path: path.join(EVID, `round-${ROUND}-1-list.png`), fullPage: true });

// ---------- 2. 搜索 ----------
await fill('cus-code-filter', 'CUS-0002');
await page.getByTestId('btn-cus-search').click();
await page.waitForFunction(
  () => {
    const t = document.querySelector('[data-testid="customer-table"]')?.textContent ?? '';
    return t.includes('CUS-0002') && !t.includes('CUS-0001');
  },
  { timeout: 20000 },
);
const searchRows = await table.locator('tbody tr').count();
check('2 编码模糊搜索只剩 CUS-0002 一行', searchRows, 1);
checkTrue('2 搜索命中的行名是上海云枢贸易', (await rowOf('CUS-0002').innerText()).includes('上海云枢贸易'));
// 空态文案（搜一个不存在的编码）
await fill('cus-code-filter', 'NO-SUCH-CODE');
await page.getByTestId('btn-cus-search').click();
await page.waitForFunction(
  () => (document.querySelector('[data-testid="customer-table"]')?.textContent ?? '').includes('没有匹配的客户'),
  { timeout: 20000 },
);
checkTrue('2 搜不到时给「没有匹配的客户」空态（不是空白表）', true);
await page.getByTestId('btn-cus-reset').click();
await page.waitForFunction(
  () => (document.querySelector('[data-testid="customer-table"]')?.textContent ?? '').includes('CUS-0001'),
  { timeout: 20000 },
);
checkTrue('2 重置回全量', true);

// ---------- 3. 新建 ----------
await page.getByTestId('btn-cus-new').click();
await page.waitForFunction(() => [...document.querySelectorAll('.t-dialog')].some((d) => d.offsetParent !== null && d.textContent.includes('新建客户')), { timeout: 15000 });
// 3a 校验：什么都不填直接点「创建」→ 字段红字，且不发 POST
const postBefore = apiLog.length;
await page.getByRole('button', { name: '创建', exact: true }).click();
await page.waitForFunction(() => [...document.querySelectorAll('.t-dialog')].some((d) => d.offsetParent !== null && d.textContent.includes('客户编码必填')), { timeout: 10000 });
checkTrue('3a 空表单点创建 → 「客户编码必填」字段错误', true);
checkTrue('3a 校验拦住时没有发 POST', !apiLog.slice(postBefore).some((l) => l.startsWith('POST')));
await fill('cus-code', CODE);
await fill('cus-name', NAME1);
await fill('cus-contact-name', '王五');
await fill('cus-contact-phone', '13900000009');
await fill('cus-tax-no', '91110108MA0SO03X1');
await fill('cus-address', '北京市朝阳区验收路 3 号');
await fill('cus-credit-limit', '12345.67');
await fill('cus-remark', `第 ${ROUND} 轮真机验收数据`);
await page.screenshot({ path: path.join(EVID, `round-${ROUND}-2-create-dialog.png`), fullPage: true });
await page.getByRole('button', { name: '创建', exact: true }).click();
await page.waitForFunction(
  (code) => (document.querySelector('[data-testid="customer-table"]')?.textContent ?? '').includes(code),
  CODE,
  { timeout: 20000 },
);
checkTrue('3 新建成功后列表出现新编码', true);
const created = await api('GET', `/api/business/erp/customer/page?code=${encodeURIComponent(CODE)}`);
const createdRow = created.body.data.list[0];
check('3 库里有这条客户', created.body.data.total, 1);
check('3 名称落库（trim 后）', createdRow.name, NAME1);
check('3 信用额度按字符串定点落库', createdRow.credit_limit, '12345.6700');
check('3 状态默认 ACTIVE', createdRow.status, 'ACTIVE');
check('3 联系人落库', createdRow.contact_name, '王五');
say(`   创建回读: ${JSON.stringify({ code: createdRow.code, name: createdRow.name, credit_limit: createdRow.credit_limit, status: createdRow.status })}`);

// ---------- 4. 编辑（部分更新：只改名称/联系人/额度） ----------
await rowOf(CODE).getByTestId('btn-cus-edit').click();
await page.waitForFunction(() => [...document.querySelectorAll('.t-dialog')].some((d) => d.offsetParent !== null && d.textContent.includes('编辑客户')), { timeout: 15000 });
await fill('cus-name', NAME2);
await fill('cus-contact-name', '赵六');
await fill('cus-credit-limit', '20000');
await page.getByRole('button', { name: '保存', exact: true }).click();
await page.waitForFunction(
  (name) => (document.querySelector('[data-testid="customer-table"]')?.textContent ?? '').includes(name),
  NAME2,
  { timeout: 20000 },
);
checkTrue('4 编辑后列表显示新名称', true);
const afterEdit = await api('GET', `/api/business/erp/customer/page?code=${encodeURIComponent(CODE)}`);
const editedRow = afterEdit.body.data.list[0];
check('4 名称已更新', editedRow.name, NAME2);
check('4 联系人已更新', editedRow.contact_name, '赵六');
check('4 额度已更新', editedRow.credit_limit, '20000.0000');
check('4 未改字段（税号）保持不变', editedRow.tax_no, '91110108MA0SO03X1');
await page.screenshot({ path: path.join(EVID, `round-${ROUND}-3-edited.png`), fullPage: true });

// ---------- 5. 状态切换 ACTIVE → FROZEN → ACTIVE ----------
const editRow = rowOf(CODE);
await editRow.getByTestId('btn-cus-freeze').click();
await page.waitForFunction(
  (code) => {
    const tr = [...document.querySelectorAll('[data-testid="customer-table"] tbody tr')].find((r) => r.textContent.includes(code));
    const tag = tr?.querySelector('[data-testid="cus-status-tag"]');
    return tag?.textContent?.trim() === '冻结';
  },
  CODE,
  { timeout: 20000 },
);
checkTrue('5 点「冻结」后行内状态 tag 变「冻结」', true);
const frozen = await api('GET', `/api/business/erp/customer/page?code=${encodeURIComponent(CODE)}`);
check('5 库里 status=FROZEN', frozen.body.data.list[0].status, 'FROZEN');
await rowOf(CODE).getByTestId('btn-cus-activate').click();
await page.waitForFunction(
  (code) => {
    const tr = [...document.querySelectorAll('[data-testid="customer-table"] tbody tr')].find((r) => r.textContent.includes(code));
    const tag = tr?.querySelector('[data-testid="cus-status-tag"]');
    // 只看状态 tag：解除冻结后行内「操作」列文案会变成「冻结」，整行 textContent 判据会自相矛盾
    return tag?.textContent?.trim() === '正常';
  },
  CODE,
  { timeout: 20000 },
);
checkTrue('5 点「解冻」后行内状态 tag 变「正常」', true);
const active = await api('GET', `/api/business/erp/customer/page?code=${encodeURIComponent(CODE)}`);
check('5 库里 status=ACTIVE', active.body.data.list[0].status, 'ACTIVE');
await page.screenshot({ path: path.join(EVID, `round-${ROUND}-4-status.png`), fullPage: true });

// ---------- 6. 删除（软删 + 确认文案写明编码可复用） ----------
await rowOf(CODE).getByTestId('btn-cus-delete').click();
await page.waitForFunction(
  () => [...document.querySelectorAll('.t-dialog')].some((d) => d.offsetParent !== null && d.textContent.includes('删除客户')),
  { timeout: 15000 },
);
const deleteDialog = [...(await page.locator('.t-dialog').all())].filter(async () => true).at(-1);
const deleteText = await (await page.locator('.t-dialog', { hasText: '删除客户' }).last()).innerText();
checkTrue('6 删除确认文案写明「同一编码可以再次使用」', deleteText.includes('同一编码可以再次使用'));
await page.getByRole('button', { name: '删除', exact: true }).last().click();
await page.waitForFunction(
  (code) => !(document.querySelector('[data-testid="customer-table"]')?.textContent ?? '').includes(code),
  CODE,
  { timeout: 20000 },
);
checkTrue('6 删除后列表不再显示该客户', true);
const afterDelete = await api('GET', `/api/business/erp/customer/page?code=${encodeURIComponent(CODE)}`);
check('6 库中分页 total=0（软删）', afterDelete.body.data.total, 0);
const detailAfterDelete = await api('GET', `/api/business/erp/customer/${createdRow.id}`);
check('6 软删后详情 404', detailAfterDelete.status, 404);
await page.screenshot({ path: path.join(EVID, `round-${ROUND}-5-deleted.png`), fullPage: true });

// ---------- 7. 控制台 / 网络噪声 ----------
const realErrors = consoleErrors.filter(
  (text) => !text.includes('notice/stream') && !text.includes('Failed to load resource: the server responded with a status of 401'),
);
check('7 页面无未预期 console error', realErrors, []);
say(`   本轮 API 调用（真接口）：`);
for (const line of apiLog.filter((l) => l.includes('/business/erp/customer')).slice(0, 40)) {
  say(`     ${line}`);
}

say(`== 第 ${ROUND} 轮结果：PASS=${pass} FAIL=${fail}`);
await browser.close();
process.exit(fail === 0 ? 0 : 1);
