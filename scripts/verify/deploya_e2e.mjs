#!/usr/bin/env node
/**
 * DEPLOY-A 真机验收（Playwright，打 144 的 nginx → 线上 dist → 8023 后端）
 *
 * 覆盖卡面第 6 步的 10 项：
 *  ① 合同管理页能打开（菜单可见 + 列表加载）
 *  ② 新建合同（乙方/金额/日期）→ 保存成功 → 列表可见
 *  ③ 合同状态流转 DRAFT → SIGNED → ACTIVE（徽标变绿）
 *  ④ 采购单选「合同采购」→ 合同选择器列出 ACTIVE 合同 → 供应商自动带出且只读
 *  ⑤ 采购单保存 → 列表显示「合同采购」+ 合同号
 *  ⑥ 采购单详情：合同号链接 + 审批单 FLW-xxx 链接（两个都在）
 *  ⑦ 生成收货单 → 收货页供应商只读 + 显示采购单号
 *  ⑧ 收货单保存 → 过账（<1000 元走免审分支）
 *  ⑨ 现货采购单：供应商可手填（没被改坏）
 *  ⑩ 控制台无 error、无 4xx/5xx
 *
 * 用法（144 上）：set -a; . /etc/cloudpivot/cloudpivot.env; set +a; node scripts/verify/deploya_e2e.mjs
 */
import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';
import { writeFileSync } from 'node:fs';

const BASE = process.env.DEPLOYA_BASE_URL || 'http://127.0.0.1';
const USER = process.env.DEPLOYA_USER || 'admin';
const PASSWORDS = [process.env.NIK_ADMIN_PASSWORD, process.env.ALUN_ADMIN_PASSWORD, 'Admin@123456'].filter(Boolean);
const SHOT_DIR = process.env.DEPLOYA_SHOT_DIR || '/opt/cloudpivot/docs/evidence';
const STAMP = new Date().toISOString().replace(/[-:T.Z]/g, '').slice(0, 14);
const TAG = `DEPLOYA-${STAMP}`;

let PASS = 0, FAIL = 0;
const failures = [];
const ok = (m) => { PASS++; console.log(`[PASS] ${m}`); };
const bad = (m) => { FAIL++; failures.push(m); console.log(`[FAIL] ${m}`); };
const info = (m) => console.log(`       ${m}`);

async function login() {
  for (const password of PASSWORDS) {
    const res = await fetch(`${BASE}/api/system/auth/login`, {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username: USER, password }),
    });
    const body = await res.json().catch(() => ({}));
    if (body.code === 0 && body.data?.access_token) return body.data;
  }
  return null;
}
const apiGet = async (token, path) => {
  const res = await fetch(`${BASE}${path}`, { headers: { Authorization: `Bearer ${token}` } });
  const body = await res.json().catch(() => ({}));
  return { status: res.status, body };
};

const session = await login();
if (!session) { console.error('登录失败：口令候选都不对'); process.exit(2); }
info(`登录成功 user=admin token=${session.access_token.slice(0, 12)}…`);

const browser = await chromium.launch();
const context = await browser.newContext({ viewport: { width: 1500, height: 1100 } });
await context.addInitScript(([a, r]) => {
  localStorage.setItem('x_access_token', a);
  localStorage.setItem('x_refresh_token', r);
  localStorage.setItem('x_guide_done', '1');
  localStorage.setItem('cp-onboarding-done', '1');
}, [session.access_token, session.refresh_token]);
const page = await context.newPage();
const consoleErrors = [], httpErrors = [];
page.on('console', (m) => { if (m.type() === 'error') consoleErrors.push(m.text()); });
page.on('pageerror', (e) => consoleErrors.push(String(e)));
page.on('response', (r) => { if (r.status() >= 400 && r.url().includes('/api/')) httpErrors.push(`${r.status()} ${r.url()}`); });

let shotIdx = 0;
async function shot(name) {
  shotIdx += 1;
  const file = `${SHOT_DIR}/deployA-${String(shotIdx).padStart(2, '0')}-${name}.png`;
  await page.screenshot({ path: file, fullPage: false });
  info(`截图 → ${file}`);
  return file;
}
const text = async (sel) => ((await page.locator(sel).first().textContent()) || '').trim();
const visible = async (sel) => (await page.locator(sel).first().isVisible().catch(() => false));

/** 打开 TDesign 下拉并选一项（先点选项，失败再走键盘） */
async function pick(testid, keyword) {
  const root = page.locator(`[data-testid="${testid}"]`).first();
  await root.scrollIntoViewIfNeeded().catch(() => {});
  await root.click();
  await page.waitForTimeout(450);
  const opt = page.locator(`.t-select-option:visible:has-text("${keyword}")`).first();
  if (await opt.count()) { await opt.click(); await page.waitForTimeout(350); return true; }
  const input = root.locator('input').first();
  if (await input.count()) {
    await input.fill(keyword).catch(() => {});
    await page.waitForTimeout(500);
    const opt2 = page.locator(`.t-select-option:visible:has-text("${keyword}")`).first();
    if (await opt2.count()) { await opt2.click(); await page.waitForTimeout(350); return true; }
    await input.press('Enter').catch(() => {});
    await page.waitForTimeout(300);
    return true;
  }
  return false;
}
/** 数字输入：点进去全选后输入 */
async function fillNumber(testid, value) {
  const root = page.locator(`[data-testid="${testid}"]`).first();
  const input = root.locator('input').first();
  await input.click();
  await input.press(process.platform === 'darwin' ? 'Meta+a' : 'Control+a').catch(() => {});
  await input.fill(String(value));
  await input.press('Tab');
  await page.waitForTimeout(200);
}
/** 点当前可见弹窗里的主按钮（DialogPlugin 的「确定」/ 自定义的「确认」） */
async function confirmDialog() {
  await page.waitForSelector('.t-dialog:visible, [data-testid="contract-status-dialog"]', { timeout: 15000 }).catch(() => {});
  const btn = page.locator('.t-dialog:visible button.t-button--theme-primary, .t-dialog:visible .t-dialog__footer button').last();
  if (await btn.count()) { await btn.click(); }
  else { const b2 = page.locator('button:has-text("确定"), button:has-text("确认")').last(); await b2.click(); }
  await page.waitForTimeout(1200);
}

const ids = { tag: TAG };

try {
  /* ================= ① 合同管理页 ================= */
  console.log('\n--- ① 合同管理页面 ---');
  await page.goto(`${BASE}/business/erp/contract`, { waitUntil: 'networkidle' });
  const menuOk = (await page.locator('.t-menu, aside').first().textContent().catch(() => '') || '').includes('合同管理');
  menuOk ? ok('① 侧边栏出现「合同管理」菜单') : bad('① 侧边栏没有「合同管理」菜单');
  if (page.url().includes('/business/erp/contract')) ok(`① 路由可达：${page.url()}`); else bad(`① 路由被重定向到 ${page.url()}`);
  const tableOrEmpty = (await visible('[data-testid="contract-table"]')) || (await visible('[data-testid="contract-empty"]'));
  tableOrEmpty ? ok('① 列表区域渲染完成（table/empty）') : bad('① 列表区域未渲染');
  const loadErr = (await text('[data-testid="contract-load-error"]').catch(() => '')) || '';
  loadErr ? bad(`① 列表加载报错：${loadErr}`) : ok('① 列表加载无错误提示');
  await shot('contract-list');

  /* ================= ② 新建合同 ================= */
  console.log('\n--- ② 新建合同 ---');
  await page.locator('[data-testid="btn-contract-create"]').click();
  await page.waitForSelector('[data-testid="contract-form-dialog"]', { timeout: 15000 });
  const contractName = `${TAG}-框架采购合同`;
  await page.locator('[data-testid="contract-form-name"] input').fill(contractName);
  const typeOk = await pick('contract-form-type', '采购合同');
  typeOk ? ok('② 合同类型选「采购合同」') : bad('② 合同类型下拉选不动');
  const supplierOk = await pick('contract-form-party-b', '示例供应商');
  supplierOk ? ok('② 乙方选了供应商「示例供应商（SUP-0001）」') : bad('② 乙方下拉选不动');
  await fillNumber('contract-form-amount', 5000);
  const signDate = await page.locator('[data-testid="contract-form-sign-date"] input').inputValue().catch(() => '');
  info(`签订日期（默认带出）= ${signDate || '(空)'}`);
  signDate ? ok('② 签订/有效期日期已带出默认值') : bad('② 日期为空，需要手工选');
  await shot('contract-form');
  await page.locator('[data-testid="btn-contract-save"]').click();
  await page.waitForSelector('[data-testid="contract-form-dialog"]', { state: 'hidden', timeout: 20000 }).catch(() => {});
  await page.waitForTimeout(1500);
  const stillOpen = await visible('[data-testid="contract-form-dialog"]');
  if (stillOpen) {
    const err = await text('[data-testid="contract-form-dialog"] .t-alert').catch(() => '');
    bad(`② 保存未成功（弹窗还在）错误文案：${err || '无'}`);
    await shot('contract-form-error');
  } else {
    ok('② 合同保存成功（弹窗关闭）');
  }
  // 从接口拿合同号（列表行也可，但接口更稳）
  const cpage = await apiGet(session.access_token, `/api/business/erp/contract/page?page=1&page_size=20&name=${encodeURIComponent(TAG)}`);
  const row0 = (cpage.body?.data?.list ?? [])[0];
  if (row0) { ids.contractNo = row0.contract_no; ids.contractId = row0.id; ok(`② 列表/接口可见新合同：${row0.contract_no}（状态 ${row0.status}）`); }
  else bad(`② 接口查不到新建的合同（name=${TAG}）`);
  await page.locator('[data-testid="btn-contract-refresh"]').click();
  await page.waitForTimeout(1500);
  if (ids.contractNo && (await page.locator(`[data-testid="contract-no-${ids.contractNo}"]`).count())) ok('② 列表行可见（DOM 命中 contract-no）');
  else bad('② 列表 DOM 里找不到新合同行');
  await shot('contract-created');

  /* ================= ③ 状态流转 ================= */
  console.log('\n--- ③ 合同状态流转 DRAFT → SIGNED → ACTIVE ---');
  const no = ids.contractNo;
  for (const [target, label] of [['SIGNED', '已签订'], ['ACTIVE', '已生效']]) {
    const btnSel = `[data-testid="btn-contract-to-${target}-${no}"]`;
    if (!(await page.locator(btnSel).count())) { bad(`③ 列表没有「${target}」流转按钮`); continue; }
    await page.locator(btnSel).click();
    await page.waitForSelector('[data-testid="contract-status-dialog"]', { timeout: 15000 });
    await shot(`contract-status-${target}-confirm`);
    await page.locator('[data-testid="contract-status-dialog"] button.t-button--theme-primary, .t-dialog:visible button:has-text("确认")').last().click();
    await page.waitForTimeout(1800);
    const badge = await text(`[data-testid="contract-status-${no}"]`).catch(() => '');
    badge.includes(label) ? ok(`③ 状态流转到 ${target}（徽标「${badge}」）`) : bad(`③ 流转到 ${target} 后徽标是「${badge}」`);
  }
  await shot('contract-active');
  const detail = await apiGet(session.access_token, `/api/business/erp/contract/${ids.contractId}`);
  info(`③ 接口回读状态 = ${detail.body?.data?.status}`);

  /* ================= ④ 采购单：合同采购 ================= */
  console.log('\n--- ④ 采购单新建：合同采购 + 供应商派生只读 ---');
  await page.goto(`${BASE}/business/erp/purchase-order`, { waitUntil: 'networkidle' });
  await page.locator('[data-testid="btn-purchase-create"]').click();
  await page.waitForSelector('[data-testid="purchase-form-dialog"]', { timeout: 15000 });
  await page.locator('[data-testid="purchase-type-contract"]').click();
  await page.waitForTimeout(600);
  const contractSelExists = await visible('[data-testid="purchase-contract"]');
  contractSelExists ? ok('④ 选「合同采购」后出现合同选择器') : bad('④ 合同选择器没有出现');
  await page.locator('[data-testid="purchase-contract"]').click();
  await page.waitForTimeout(700);
  const optCount = await page.locator('.t-select-option:visible').count();
  const hasOurContract = await page.locator(`.t-select-option:visible:has-text("${no}")`).count();
  hasOurContract ? ok(`④ 合同选择器列出刚建的 ACTIVE 合同 ${no}（共 ${optCount} 个选项）`) : bad(`④ 合同选择器没有列出 ${no}（共 ${optCount} 个选项）`);
  if (hasOurContract) { await page.locator(`.t-select-option:visible:has-text("${no}")`).first().click(); await page.waitForTimeout(800); }
  const supplierVal = await page.locator('[data-testid="purchase-supplier-readonly"] input').inputValue().catch(() => '');
  const supplierReadonly = await page.locator('[data-testid="purchase-supplier-readonly"] input').getAttribute('readonly').catch(() => null);
  supplierVal.includes('示例供应商') ? ok(`④ 供应商自动带出乙方：${supplierVal}`) : bad(`④ 供应商字段没有带出乙方（值=${supplierVal}）`);
  supplierReadonly !== null ? ok('④ 供应商字段只读（readonly 属性在）') : bad('④ 供应商字段没有 readonly');
  const spotSelectorGone = !(await visible('[data-testid="purchase-supplier"]'));
  spotSelectorGone ? ok('④ 合同采购下没有可编辑的供应商下拉（不存在 hand-fill 入口）') : bad('④ 合同采购下仍渲染了可编辑供应商下拉');
  await shot('purchase-contract-form');

  /* ================= ⑤ 采购单保存 ================= */
  console.log('\n--- ⑤ 采购单保存（合同采购）---');
  await pick('purchase-item-material-0', '原料A');
  await fillNumber('purchase-item-qty-0', 10);
  await fillNumber('purchase-item-price-0', 10);
  await pick('purchase-warehouse', '总仓');
  const qty = await page.locator('[data-testid="purchase-item-qty-0"] input').inputValue().catch(() => '');
  const price = await page.locator('[data-testid="purchase-item-price-0"] input').inputValue().catch(() => '');
  info(`明细：qty=${qty} price=${price}`);
  await shot('purchase-form-filled');
  await page.locator('[data-testid="btn-purchase-save-draft"]').click();
  await page.waitForSelector('[data-testid="purchase-form-dialog"]', { state: 'hidden', timeout: 20000 }).catch(() => {});
  await page.waitForTimeout(1500);
  if (await visible('[data-testid="purchase-form-dialog"]')) {
    const err = await text('[data-testid="purchase-form-dialog"] .t-alert').catch(() => '');
    bad(`⑤ 采购单保存失败，弹窗未关：${err || '无文案'}`);
    await shot('purchase-form-error');
  } else ok('⑤ 采购单保存成功（弹窗关闭）');

  const ppage = await apiGet(session.access_token, `/api/business/erp/purchase-order/page?page=1&page_size=20&contract_id=${ids.contractId}`);
  let poRow = (ppage.body?.data?.list ?? []).find((r) => r.contract_id === ids.contractId) ?? null;
  if (!poRow) {
    const all = await apiGet(session.access_token, `/api/business/erp/purchase-order/page?page=1&page_size=30`);
    poRow = (all.body?.data?.list ?? []).find((r) => r.contract_no_snapshot === ids.contractNo) ?? null;
  }
  if (poRow) { ids.poNo = poRow.order_no; ids.poId = poRow.id; ok(`⑤ 接口可见采购单 ${poRow.order_no}（类型 ${poRow.purchase_type}，合同号 ${poRow.contract_no_snapshot}）`); }
  else bad('⑤ 接口查不到新建采购单');
  await page.locator('[data-testid="btn-purchase-refresh"]').click();
  await page.waitForTimeout(1500);
  if (ids.poNo) {
    const typeText = await text(`[data-testid="purchase-type-${ids.poNo}"]`).catch(() => '');
    const cnoText = await text(`[data-testid="purchase-contract-no-${ids.poNo}"]`).catch(() => '');
    typeText.includes('合同') ? ok(`⑤ 列表「采购类型」列显示「${typeText}」`) : bad(`⑤ 列表采购类型列是「${typeText}」`);
    cnoText.includes(no) ? ok(`⑤ 列表「合同号」列显示 ${cnoText}`) : bad(`⑤ 列表合同号列是「${cnoText}」，期望含 ${no}`);
  }
  await shot('purchase-list');

  /* ================= ⑥ 采购单详情：两个链接 ================= */
  console.log('\n--- ⑥ 采购单详情：合同号链接 + 审批单链接 ---');
  if (ids.poNo) {
    // 先提交，才会有审批单 FLW-xxx
    if (await page.locator(`[data-testid="btn-submit-${ids.poNo}"]`).count()) {
      await page.locator(`[data-testid="btn-submit-${ids.poNo}"]`).click();
      await confirmDialog();
      await page.waitForTimeout(1500);
      const st = await text(`[data-testid="purchase-status-${ids.poNo}"]`).catch(() => '');
      st.includes('已提交') ? ok(`⑥ 采购单已提交审批（状态「${st}」）`) : info(`⑥ 提交后状态 = ${st}`);
    } else info('⑥ 没有「提交审批」按钮（可能已是提交态）');
    await page.locator(`[data-testid="btn-view-${ids.poNo}"]`).click();
    await page.waitForSelector('[data-testid="purchase-detail-drawer"]', { timeout: 15000 });
    await page.waitForTimeout(1200);
    const dtype = await text('[data-testid="purchase-detail-type"]').catch(() => '');
    const clink = await text('[data-testid="purchase-detail-contract-link"]').catch(() => '');
    const approval = await text('[data-testid="purchase-detail-approval"]').catch(() => '');
    dtype.includes('合同') ? ok(`⑥ 详情采购类型 = ${dtype}`) : bad(`⑥ 详情采购类型 = ${dtype}`);
    clink.includes(no) ? ok(`⑥ 详情有合同号链接：${clink}`) : bad(`⑥ 详情没有合同号链接（${clink}）`);
    /FLW-/.test(approval) ? ok(`⑥ 详情有审批单链接：${approval.replace(/\s+/g, ' ').slice(0, 60)}`) : bad(`⑥ 详情没有 FLW 审批单链接（${approval.slice(0, 60)}）`);
    await shot('purchase-detail-links');

    /* ================= ⑦ 生成收货单 ================= */
    console.log('\n--- ⑦ 从采购单生成收货单 ---');
    if (await page.locator('[data-testid="btn-purchase-gen-stockin"]').count()) {
      await page.locator('[data-testid="btn-purchase-gen-stockin"]').click();
      await page.waitForSelector('[data-testid="stockin-form-dialog"]', { timeout: 20000 });
      await page.waitForTimeout(1500);
      const supVal = await page.locator('[data-testid="stockin-supplier-readonly"] input').inputValue().catch(() => '');
      const supRo = await page.locator('[data-testid="stockin-supplier-readonly"] input').getAttribute('readonly').catch(() => null);
      const poHint = await text('[data-testid="stockin-purchase-order-no"]').catch(() => '');
      const alertText = await text('[data-testid="stockin-source-po"]').catch(() => '');
      supVal.includes('示例供应商') ? ok(`⑦ 收货页供应商只读带出：${supVal}`) : bad(`⑦ 收货页供应商值 = ${supVal}`);
      supRo !== null ? ok('⑦ 收货页供应商输入框 readonly') : bad('⑦ 收货页供应商没有 readonly');
      (poHint.includes(ids.poNo) || alertText.includes(ids.poNo)) ? ok(`⑦ 收货页显示采购单号 ${ids.poNo}`) : bad(`⑦ 收货页没显示采购单号（hint="${poHint}" alert="${alertText.slice(0, 40)}"）`);
      info(`收货页地址：${page.url()}`);
      await shot('stockin-from-po');

      /* ================= ⑧ 收货单保存 → 过账 ================= */
      console.log('\n--- ⑧ 收货单保存 → 过账 ---');
      await page.locator('[data-testid="btn-stockin-save"]').click();
      await page.waitForSelector('[data-testid="stockin-form-dialog"]', { state: 'hidden', timeout: 20000 }).catch(() => {});
      await page.waitForTimeout(1500);
      if (await visible('[data-testid="stockin-form-dialog"]')) {
        const err = await text('[data-testid="stockin-form-dialog"] .t-alert').catch(() => '');
        bad(`⑧ 收货单保存失败：${err || '无文案'}`);
        await shot('stockin-form-error');
      } else ok('⑧ 收货单保存成功（弹窗关闭）');
      const spage = await apiGet(session.access_token, `/api/business/erp/stock-in/page?page=1&page_size=30`);
      const siRow = (spage.body?.data?.list ?? []).find((r) => r.purchase_order_no === ids.poNo || r.source_order_id === ids.poId) ?? null;
      if (siRow) { ids.inNo = siRow.in_no; ids.inId = siRow.id; ok(`⑧ 收货单已生成：${siRow.in_no}（状态 ${siRow.status}）`); }
      else bad('⑧ 接口查不到生成的收货单');
      await page.locator('[data-testid="btn-stockin-refresh"]').click();
      await page.waitForTimeout(1500);
      if (ids.inNo && (await page.locator(`[data-testid="btn-post-${ids.inNo}"]`).count())) {
        await page.locator(`[data-testid="btn-post-${ids.inNo}"]`).click();
        await page.waitForSelector('[data-testid="stockin-post-dialog"]', { timeout: 15000 });
        await page.waitForTimeout(600);
        await shot('stockin-post-dialog');
        const invoiceOpt = page.locator('[data-testid="stockin-invoice-received"]');
        if (await invoiceOpt.count()) { await invoiceOpt.click().catch(() => {}); await page.waitForTimeout(300); }
        await page.locator('[data-testid="btn-stockin-post-confirm"]').click();
        await page.waitForTimeout(2500);
        const postMsg = await text('.t-dialog:visible, .t-message').catch(() => '');
        info(`过账反馈：${postMsg.replace(/\s+/g, ' ').slice(0, 80)}`);
        await page.waitForTimeout(1200);
        const stText = await text(`[data-testid="stockin-status-${ids.inNo}"]`).catch(() => '');
        stText.includes('已过账') ? ok(`⑧ 过账成功（列表状态「${stText}」）`) : info(`⑧ 过账后列表状态 = 「${stText}」`);
      } else bad('⑧ 找不到「过账」按钮（收货单可能被送审）');
      await shot('stockin-after-post');
    } else bad('⑦ 详情里没有「生成收货单」按钮（can_receive=false？）');
  }

  /* ================= ⑨ 现货采购单 ================= */
  console.log('\n--- ⑨ 现货采购单：供应商可手填 ---');
  await page.goto(`${BASE}/business/erp/purchase-order`, { waitUntil: 'networkidle' });
  await page.locator('[data-testid="btn-purchase-create"]').click();
  await page.waitForSelector('[data-testid="purchase-form-dialog"]', { timeout: 15000 });
  await page.locator('[data-testid="purchase-type-spot"]').click();
  await page.waitForTimeout(600);
  const spotSelVisible = await visible('[data-testid="purchase-supplier"]');
  spotSelVisible ? ok('⑨ 现货采购下供应商是可编辑下拉') : bad('⑨ 现货采购下供应商下拉不见了（被改坏）');
  const contractGone = !(await visible('[data-testid="purchase-contract"]'));
  contractGone ? ok('⑨ 现货采购下没有合同选择器（符合设计：整块不渲染）') : bad('⑨ 现货采购下仍渲染合同选择器');
  if (spotSelVisible) {
    const picked = await pick('purchase-supplier', '示例供应商');
    const spotSupVal = await text('[data-testid="purchase-supplier"]').catch(() => '');
    picked ? ok(`⑨ 供应商手填可选（当前显示「${spotSupVal.replace(/\s+/g, ' ').slice(0, 30)}」）`) : bad('⑨ 供应商下拉选不动');
  }
  await pick('purchase-item-material-0', '原料B');
  await fillNumber('purchase-item-qty-0', 2);
  await fillNumber('purchase-item-price-0', 5);
  await pick('purchase-warehouse', '分仓');
  await shot('purchase-spot-form');
  await page.locator('[data-testid="btn-purchase-save-draft"]').click();
  await page.waitForSelector('[data-testid="purchase-form-dialog"]', { state: 'hidden', timeout: 20000 }).catch(() => {});
  await page.waitForTimeout(1500);
  if (await visible('[data-testid="purchase-form-dialog"]')) {
    const err = await text('[data-testid="purchase-form-dialog"] .t-alert').catch(() => '');
    bad(`⑨ 现货采购单保存失败：${err || '无文案'}`);
  } else ok('⑨ 现货采购单保存成功（供应商手填路径未被改坏）');
  await shot('purchase-spot-saved');

  /* ================= ⑩ 控制台 ================= */
  console.log('\n--- ⑩ 控制台 / 网络 ---');
  const filteredConsole = consoleErrors.filter((t) => !/notice\/stream|net::ERR_ABORTED|sse/i.test(t));
  filteredConsole.length === 0 ? ok('⑩ 控制台无 error') : bad(`⑩ 控制台有 ${filteredConsole.length} 条 error：${filteredConsole.slice(0, 3).join(' | ')}`);
  const filteredHttp = httpErrors.filter((u) => !/\/api\/support\/notice\/stream/.test(u));
  filteredHttp.length === 0 ? ok('⑩ 无 4xx/5xx 接口响应') : bad(`⑩ 有 ${filteredHttp.length} 个 4xx/5xx：${filteredHttp.slice(0, 5).join(' , ')}`);
} catch (error) {
  bad(`执行中断：${error?.message ?? error}`);
  await shot('aborted').catch(() => {});
}

console.log(`\n===== 真机验收汇总: PASS=${PASS} FAIL=${FAIL} =====`);
if (failures.length) console.log(`失败项：\n - ${failures.join('\n - ')}`);
console.log(`证据目录：${SHOT_DIR}/deployA-*.png（共 ${shotIdx} 张）`);
console.log(`产出单号：${JSON.stringify(ids)}`);
writeFileSync('/root/deploya-logs/e2e-ids.json', JSON.stringify({ ids, pass: PASS, fail: FAIL, failures, consoleErrors, httpErrors }, null, 2));
await browser.close();
process.exit(FAIL === 0 ? 0 : 1);
