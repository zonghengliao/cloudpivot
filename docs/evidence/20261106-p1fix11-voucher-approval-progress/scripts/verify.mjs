// P1-FIX-11 真机验收：凭证详情的「审批进度」是否按 pending_reverse_instance_id 渲染
// 跑法：node verify.mjs   （node_modules 软链到 /opt/cloudpivot/frontend/node_modules）
// 两段：
//   A. 红冲审批中的凭证（PZ-20261103-000540）→ 抽屉应出现「审批进度」+ 节点 + 审批人
//   B. 对照组：没有待办实例的 POSTED 凭证 → 不该出现进度块、也不该发 flow/instance 请求
import { chromium } from '@playwright/test';
import fs from 'node:fs';
import path from 'node:path';

const BASE = process.env.BASE || 'http://127.0.0.1:5186';
const TOK = fs.readFileSync('/root/fix11-run/token.txt', 'utf8').trim();
const LOGIN = JSON.parse(fs.readFileSync('/root/fix11-run/login.json', 'utf8'));
const REFRESH = LOGIN.data.refresh_token;
const VNO = process.env.VNO || 'PZ-20261103-000540';          // 红冲审批中
const VNO2 = process.env.VNO2 || 'PZ-20261102-001579';        // 对照：无审批实例
const INST = process.env.INST || '01a1acb0-40f8-7308-a4f7-09b14c5e587c';
const OUT = process.env.OUT || '/root/fix11-run/shots';
fs.mkdirSync(OUT, { recursive: true });

const ok = [];
const bad = [];
function check(name, cond, detail) {
  (cond ? ok : bad).push({ name, detail });
  console.log(`${cond ? 'PASS' : 'FAIL'}  ${name}  ${detail === undefined ? '' : JSON.stringify(detail)}`);
}

const browser = await chromium.launch({ headless: true });
const ctx = await browser.newContext({ viewport: { width: 2048, height: 1150 } });
await ctx.addInitScript(([t, r]) => {
  try {
    localStorage.setItem('x_access_token', t);
    localStorage.setItem('x_refresh_token', r);
    localStorage.setItem('cp-onboarding-done', '1');
    sessionStorage.setItem('login-unread-shown', '1');
  } catch { /* 首帧 origin 未定，忽略 */ }
}, [TOK, REFRESH]);

const page = await ctx.newPage();
const api = [];
page.on('request', (req) => {
  const u = req.url();
  if (u.includes('/api/')) api.push(`${req.method()} ${u.replace(BASE, '')}`);
});
page.on('console', (m) => { if (m.type() === 'error') console.log('  [console.error]', m.text().slice(0, 200)); });

await page.goto(`${BASE}/business/erp/voucher`, { waitUntil: 'domcontentloaded' });

// 列表默认按期间分页，目标单不一定在第一页 —— 统一先按凭证号（模糊）筛选，保证可复现
async function searchVoucher(no) {
  const input = page.locator('[data-testid="voucher-filter-no"] input');
  await input.fill(no);
  await page.locator('[data-testid="btn-voucher-search"]').click();
  await page.waitForFunction(
    (n) => (document.querySelector('[data-testid="voucher-table"]')?.textContent ?? '').includes(n),
    no, { timeout: 30000 },
  );
  await page.waitForTimeout(400);
}

await searchVoucher(VNO);
await page.screenshot({ path: path.join(OUT, '01-list.png') });

// 列表行的「红冲审批中」徽标
const badge = await page.evaluate((no) => {
  const el = document.querySelector(`[data-testid="voucher-pending-reverse-${no}"]`);
  return el ? { text: el.textContent.trim(), visible: el.offsetParent !== null } : null;
}, VNO);
check('列表行显示「红冲审批中」徽标', !!badge && badge.visible, badge);

// 打开详情
api.length = 0;
await page.locator(`[data-testid="btn-voucher-view-${VNO}"]`).click();
await page.waitForSelector('[data-testid="voucher-detail"]', { state: 'visible', timeout: 20000 });
await page.waitForSelector('[data-testid="voucher-approval-progress"]', { state: 'visible', timeout: 20000 })
  .catch(() => {});
const prog = page.locator('[data-testid="voucher-approval-progress"]');
const progCount = await prog.count();
const progVisible = progCount > 0 ? await prog.first().isVisible() : false;
check('红冲凭证详情出现「审批进度」块', progVisible, { count: progCount, visible: progVisible });

const drawerText = await page.locator('[data-testid="voucher-drawer"]').innerText();
check('抽屉里有「审批进度」标题', drawerText.includes('审批进度'));
check('抽屉里有「红冲审批中」提示', drawerText.includes('红冲审批中'));

let progressText = '';
if (progVisible) {
  // 进度块在抽屉最底部：滚进视口再截图（否则拍到的是上半截）
  await prog.first().scrollIntoViewIfNeeded();
  await page.waitForTimeout(800);
  progressText = await prog.first().innerText();
  console.log('  progress.innerText =', JSON.stringify(progressText));
  for (const word of ['开始', '金额分级', '财务经理审批', '李四', '处理中']) {
    check(`进度块含「${word}」`, progressText.includes(word));
  }
  const rect = await prog.first().boundingBox();
  check('进度块在视口内（截图能拍到）', !!rect && rect.y >= 0 && rect.y + rect.height <= 1150,
    rect && { y: Math.round(rect.y), h: Math.round(rect.height) });
  await page.screenshot({ path: path.join(OUT, '02-detail-approval-progress.png') });
  await prog.first().screenshot({ path: path.join(OUT, '03-progress-block.png') }).catch(() => {});
}
check('详情请求了本凭证的审批实例', api.some((u) => u.includes(`/api/business/flow/instance/${INST}`)),
  api.filter((u) => u.includes('/flow/instance')));

// 详情里的单号 / 状态
const dStatus = await page.locator('[data-testid="voucher-detail-status"]').innerText().catch(() => '');
check('详情状态仍是「已记账」（红冲审批不落业务态）', dStatus.includes('已记账') || dStatus.includes('POSTED'), dStatus);

// ============ B. 对照组：没有待审批实例的凭证 ============
await page.locator('[data-testid="btn-voucher-drawer-close"]').click().catch(() => undefined);
await page.waitForTimeout(500);
if (await page.locator('[data-testid="voucher-drawer"]').evaluate((el) => el.classList.contains('t-drawer--open')).catch(() => false)) {
  await page.locator('.t-drawer__mask').click().catch(() => undefined);
  await page.waitForTimeout(500);
}
api.length = 0;
await searchVoucher(VNO2);
await page.locator(`[data-testid="btn-voucher-view-${VNO2}"]`).click();
await page.waitForSelector('[data-testid="voucher-detail"]', { state: 'visible', timeout: 20000 });
await page.waitForTimeout(1500);
const prog2 = await page.locator('[data-testid="voucher-approval-progress"]').count();
const drawerText2 = await page.locator('[data-testid="voucher-drawer"]').innerText();
check('对照凭证（无审批实例）不渲染进度块', prog2 === 0, { count: prog2 });
check('对照凭证抽屉里没有「审批进度」标题', !drawerText2.includes('审批进度'));
check('对照凭证不发 flow/instance 请求（省一次空请求）',
  !api.some((u) => u.includes('/flow/instance')), api.filter((u) => u.includes('/flow/instance')));
await page.screenshot({ path: path.join(OUT, '04-control-no-progress.png') });

const summary = { base: BASE, voucher: VNO, control: VNO2, instance: INST, pass: ok.length, fail: bad.length, failures: bad, progressText };
fs.writeFileSync(path.join(OUT, 'result.json'), JSON.stringify(summary, null, 2));
console.log('\n=== SUMMARY ===');
console.log(JSON.stringify({ pass: ok.length, fail: bad.length, failures: bad.map((b) => b.name) }));
await browser.close();
process.exit(bad.length ? 1 : 0);
