#!/usr/bin/env node
/**
 * ERP-R5 期初余额页面的 DOM 验收（Playwright，打 **vite dev + 隔离实例**）。
 *
 * 前置：
 *
 * ```bash
 * # 1) 隔离后端（cloudpivot_r5e @ 18098）
 * bash scripts/verify/r5_instance.sh start
 * # 2) vite dev（/api 转发到 18098）
 * cd frontend && BACKEND_ORIGIN=http://127.0.0.1:18098 pnpm dev --port 5189
 * ```
 *
 * 用法（从 frontend/ 目录跑，方便解析 node_modules）：
 *
 * ```bash
 * cd frontend && ERP_PAGE_ADMIN_PASSWORD='R5@Admin2026' node ../scripts/verify/r5_opening_page_check.mjs
 * ```
 *
 * 本脚本**自带夹具**：自己开 2027-02 期间、调接口录期初与库存期初，
 * 断言完再结账、验只读态。断言只看 DOM（data-testid / 文案 / disabled）；
 * 截图只做留痕。收尾不删夹具（隔离库是一次性的），但会打印期间编码方便重跑。
 */
import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';

const BASE = process.env.ERP_PAGE_BASE || 'http://127.0.0.1:5189';
const ADMIN = process.env.ERP_PAGE_ADMIN || 'admin';
const PASSWORD = process.env.ERP_PAGE_ADMIN_PASSWORD || 'R5@Admin2026';
const PERIOD = '2027-02';
const OUT_DIR = '/opt/cloudpivot/docs/evidence/20261101-erpr5';

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
const expect = (name, want, got) => {
  if (String(want) === String(got)) ok(`${name}（${got}）`);
  else bad(`${name}（期望 ${want}，实际 ${got}）`);
};
const expectMatch = (name, pattern, got) => {
  if (pattern.test(String(got))) ok(`${name}（${got}）`);
  else bad(`${name}（期望匹配 ${pattern}，实际 ${got}）`);
};

let TOKEN = null;
async function api(method, path, body) {
  const res = await fetch(`${BASE}${path}`, {
    method,
    headers: {
      'Content-Type': 'application/json',
      ...(TOKEN ? { Authorization: `Bearer ${TOKEN}` } : {}),
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  const text = await res.text();
  let payload = {};
  try {
    payload = text ? JSON.parse(text) : {};
  } catch {
    payload = { raw: text };
  }
  return { status: res.status, payload };
}

async function login() {
  const res = await fetch(`${BASE}/api/system/auth/login`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify({ username: ADMIN, password: PASSWORD }),
  });
  const body = await res.json();
  if (body.code !== 0 || !body.data?.access_token) {
    console.error('登录失败：', JSON.stringify(body).slice(0, 200));
    process.exit(1);
  }
  TOKEN = body.data.access_token;
  return body.data;
}

async function findPeriodRow() {
  const page = await api('GET', `/api/business/erp/period/page?page=1&page_size=100`);
  return (page.payload?.data?.list ?? []).find((item) => item.period_code === PERIOD);
}

async function setupFixture() {
  // 期间（已存在时后端 409，忽略）
  const period = await api('POST', '/api/business/erp/period', {
    period_year: 2027,
    period_month: 2,
    remark: 'R5 页面验收夹具',
  });
  console.log(`  开期间 ${PERIOD}：HTTP ${period.status}`);
  // 上一轮跑完是结账态 → 先反结账，否则期初写不进去
  const existing = await findPeriodRow();
  if (existing?.status === 'CLOSED') {
    const reopen = await api('POST', `/api/business/erp/period/${existing.id}/reopen`, {
      reason: 'R5 页面验收重跑，先反结账',
    });
    console.log(`  反结账 ${PERIOD}：HTTP ${reopen.status}`);
  }

  // 科目期初：借 1002 50000 / 贷 3001 50000
  const saved = await api('POST', '/api/business/erp/opening-balance', {
    period: PERIOD,
    entries: [
      { account_code: '1002', debit: 50000, credit: 0 },
      { account_code: '3001', debit: 0, credit: 50000 },
    ],
  });
  if (saved.status !== 200) {
    console.error('  科目期初保存失败：', JSON.stringify(saved.payload).slice(0, 300));
  }

  // 库存期初：挑一个非一物一码物料 + 一个仓库
  const materials = await api('GET', '/api/business/erp/material/page?status=1&page=1&page_size=50');
  const list = materials.payload?.data?.list ?? [];
  const material = list.find((row) => !row.serial_required) ?? list[0];
  const warehouses = await api('GET', '/api/business/erp/warehouse/page?status=1&page=1&page_size=50');
  const warehouse = (warehouses.payload?.data?.list ?? [])[0];
  if (!material || !warehouse) {
    console.error('  缺少物料 / 仓库夹具，跳过库存期初');
    return;
  }
  const inv = await api('POST', '/api/business/erp/opening-balance/inventory', {
    period: PERIOD,
    items: [{ material_id: material.id, warehouse_id: warehouse.id, qty: 100, unit_cost: 10 }],
  });
  console.log(`  库存期初 ${material.material_code} 100×10：HTTP ${inv.status}`);
}

async function closePeriod() {
  const row = await findPeriodRow();
  if (!row) return false;
  const res = await api('POST', `/api/business/erp/period/${row.id}/close`, { reason: 'R5 页面验收' });
  console.log(`  结账 ${PERIOD}：HTTP ${res.status}`);
  return res.status === 200;
}

/** 期间下拉里选中夹具期间（reload 之后会回到默认期间，必须重选） */
async function selectPeriod(page) {
  await page.locator('[data-testid="opening-period-select"]').click();
  await page.locator(`.t-select-option:has-text("${PERIOD}")`).first().click();
  await page.waitForTimeout(1200);
}

(async () => {
  const session = await login();
  await setupFixture();

  const browser = await chromium.launch();
  const context = await browser.newContext({ viewport: { width: 1440, height: 1000 } });
  await context.addInitScript(
    ([a, r]) => {
      localStorage.setItem('x_access_token', a);
      localStorage.setItem('x_refresh_token', r);
      localStorage.setItem('x_guide_done', '1');
    },
    [session.access_token, session.refresh_token],
  );
  const page = await context.newPage();
  const errors = [];
  const http401 = [];
  page.on('pageerror', (e) => errors.push(String(e)));
  page.on('console', (msg) => {
    if (msg.type() === 'error') errors.push(msg.text());
  });
  page.on('response', (res) => {
    if (res.status() === 401) http401.push(`${res.request().method()} ${res.url()}`);
  });

  const text = async (sel) => (await page.locator(sel).first().innerText()).trim();

  await page.goto(`${BASE}/business/erp/opening`, { waitUntil: 'networkidle' });
  await page.waitForSelector('[data-testid="opening-state-tag"]', { timeout: 30000 });

  // 选到夹具期间
  await selectPeriod(page);

  // ---------- 可编辑态 ----------
  expectMatch('状态标签：期初已录', /期初已录/, await text('[data-testid="opening-state-tag"]'));
  expect('借方合计', '50,000.00', await text('[data-testid="opening-total-debit"]'));
  expect('贷方合计', '50,000.00', await text('[data-testid="opening-total-credit"]'));
  expectMatch('平衡状态带 ✅', /平衡/, await text('[data-testid="opening-balance-state"]'));
  expectMatch('期初凭证号', /PZ-/, await text('[data-testid="opening-voucher-info"]'));
  expectMatch(
    '合并口径含库存期初',
    /1,000\.00/,
    await text('[data-testid="opening-combined-total"]'),
  );

  const saveDisabled = await page.locator('[data-testid="btn-opening-save"]').isDisabled();
  expect('平衡时「保存期初」可点', false, saveDisabled);

  // 把借方改掉 → 不平 → 保存按钮禁用（卡面「不平保存按钮禁用」）
  await page.locator('[data-testid="opening-debit-0"] input').fill('40000');
  await page.waitForTimeout(600);
  expectMatch(
    '改成不平后状态提示 ❌',
    /不平衡/,
    await text('[data-testid="opening-balance-state"]'),
  );
  const blocked = await page.locator('[data-testid="btn-opening-save"]').isDisabled();
  expect('不平衡时「保存期初」禁用', true, blocked);

  // 改回去
  await page.locator('[data-testid="opening-debit-0"] input').fill('50000');
  await page.waitForTimeout(600);

  // ---------- 库存 Tab ----------
  await page.locator('[data-testid="opening-tabs"] >> text=库存期初').first().click();
  await page.waitForTimeout(600);
  expect(
    '库存期初行数（1 个物料）',
    '1',
    await page.locator('[data-testid^="opening-inventory-row-"]').count(),
  );
  expectMatch(
    '库存期初合计 1,000.00',
    /1,000\.00/,
    await text('[data-testid="opening-inventory-total"]'),
  );
  expect(
    '库存期初保存按钮可点',
    false,
    await page.locator('[data-testid="btn-opening-save-inventory"]').isDisabled(),
  );
  expectMatch(
    '库存期初提示点名 1405 / 3103',
    /1405[\s\S]*3103/,
    await text('[data-testid="opening-inventory-hint"]'),
  );

  await page.screenshot({ path: `${OUT_DIR}/10-期初余额-可编辑.png`, fullPage: true });

  // ---------- 结账后只读 ----------
  if (await closePeriod()) {
    await page.reload({ waitUntil: 'networkidle' });
    await page.waitForSelector('[data-testid="opening-state-tag"]', { timeout: 30000 });
    await selectPeriod(page);
    expectMatch('结账后状态标签：已锁定', /已锁定/, await text('[data-testid="opening-state-tag"]'));
    expectMatch(
      '结账后顶部提示点名期间',
      /2027-02 已结账，期初不可改/,
      await text('[data-testid="opening-readonly-alert"]'),
    );
    // 只读 ≠ 无权限：动作按钮仍在但置灰（顶部的「已结账」横幅解释了为什么），
    // 而「添加科目」这类**破坏结构**的动作直接不渲染。
    expect(
      '结账后保存按钮置灰（动作仍在，不可用）',
      true,
      await page.locator('[data-testid="btn-opening-save"]').isDisabled(),
    );
    expect(
      '结账后「添加科目」不渲染',
      '0',
      await page.locator('[data-testid="btn-opening-add-row"]').count(),
    );
    const readonlyInput = await page.locator('[data-testid="opening-debit-0"] input').isDisabled();
    expect('结账后金额输入框 disabled', true, readonlyInput);
    await page.screenshot({ path: `${OUT_DIR}/11-期初余额-结账后只读.png`, fullPage: true });
  } else {
    bad('结账失败，只读态无法验收');
  }

  if (http401.length > 0) {
    console.log('  401 请求：');
    http401.slice(0, 6).forEach((line) => console.log(`    ${line}`));
  }
  // 只把 401 之外的真实错误算失败：token 过期后的 `/auth/refresh` 往返属于框架既有行为
  const realErrors = errors.filter((line) => !line.includes('401'));
  expect('无 console / page error（401 由框架的令牌刷新兜底）', '0', realErrors.length);
  if (realErrors.length > 0) console.log('  errors:', realErrors.slice(0, 5));

  await browser.close();

  console.log(`\n汇总：${PASS}/${PASS + FAIL} 通过`);
  process.exit(FAIL === 0 ? 0 : 1);
})();
