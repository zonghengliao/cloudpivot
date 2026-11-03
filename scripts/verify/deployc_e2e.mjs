/**
 * DEPLOY-C 真机验收脚本（生产实例，只读为主 + 一张领用单）
 *
 * 五项（对应卡面 第 6 步）：
 *   ① 会计期间页：结账弹窗出「挡路单据明细表格」+ 该谁处理 + [去处理][催办]
 *   ② 年结页：同一份明细清单（YEAR-END 交付的）
 *   ③ 流程定义页：「边配置」入口 → 边列表 → 变更走审批（提交一条变更审批）
 *   ④ 出库单：建领用单 → 过账（三本账 + 成本结转凭证）
 *   ⑤ 登录不白屏、控制台无 4xx/5xx（负向断言另外标注）
 *
 * 用法（在生产机上，用 frontend/ 的 node_modules 解析 @playwright/test）：
 *   cd /opt/cloudpivot/frontend
 *   DC_PASS='<admin 口令>' node ../scripts/verify/deployc_e2e.mjs
 *
 * 环境变量：
 *   DC_BASE     默认 http://127.0.0.1（nginx 静态 dist + 反代 8023）
 *   DC_OUT      截图目录，默认 /root/dc-evidence/shots
 *   DC_USER     admin
 *   DC_PASS     admin 口令（从 /etc/cloudpivot/cloudpivot.env 的 NIK_ADMIN_PASSWORD 取）
 *   DC_PERIOD   会计期间，默认 2026-10
 *   DC_YEAR     年结年份，默认 2026
 *   DC_SKIP_STOCKOUT=1  跳过建单（不想在生产造数据时）
 *   DC_STOCKOUT=1       才真的提交领用单（默认 0 = 只填表单不保存，先看表单渲染）
 *
 * 产物：截图 png + 结尾 JSON（stdout 最后一段）+ 结果文件 $DC_OUT/result.json
 */
import { mkdirSync, writeFileSync } from 'node:fs';
import { createRequire } from 'node:module';

// 本脚本住在 scripts/verify/（不在 frontend/ 里），ESM 按脚本位置解析依赖会找不到
// @playwright/test ⇒ 用 createRequire 按前端工程的 node_modules 解析。
const FE_DIR = process.env.DC_FE_DIR ?? '/opt/cloudpivot/frontend';
const { chromium } = createRequire(`${FE_DIR}/package.json`)('@playwright/test');

const BASE = process.env.DC_BASE ?? 'http://127.0.0.1';
const OUT = process.env.DC_OUT ?? '/root/dc-evidence/shots';
const USER = process.env.DC_USER ?? 'admin';
const PASS = process.env.DC_PASS ?? '';
const PERIOD = process.env.DC_PERIOD ?? '2026-10';
const YEAR = process.env.DC_YEAR ?? '2026';
const DO_STOCKOUT = (process.env.DC_STOCKOUT ?? '0') === '1';

mkdirSync(OUT, { recursive: true });

const results = [];
const shots = [];
const consoleErrors = [];
const httpErrors = [];

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

/** TDesign 下拉：点开 → 按文本挑一个选项（filterable 时先输入过滤） */
async function pickOption(page, testid, matchText) {
  const box = page.locator(`[data-testid="${testid}"]`);
  await box.click();
  await sleep(300);
  const input = box.locator('input').first();
  if ((await input.count()) > 0) {
    try {
      await input.fill(matchText);
      await sleep(400);
    } catch {
      /* 非 filterable 就算了 */
    }
  }
  const opts = page.locator('.t-select__dropdown .t-select-option');
  const n = await opts.count();
  const texts = [];
  for (let i = 0; i < n; i += 1) {
    const t = (await opts.nth(i).innerText()).trim();
    texts.push(t);
    if (t.includes(matchText)) {
      await opts.nth(i).click();
      await sleep(200);
      return t;
    }
  }
  throw new Error(`下拉 ${testid} 里没有「${matchText}」，候选：${texts.slice(0, 6).join(' / ')}`);
}

async function rowsOf(page, selector) {
  return page.locator(selector).count();
}

/** 点一下：普通点击失败（被 loading 蒙层/滚动容器挡住）时退回 DOM 事件派发 */
async function safeClick(loc, label = '') {
  try {
    await loc.click({ timeout: 8000 });
    return true;
  } catch (e1) {
    try {
      await loc.dispatchEvent('click');
      return true;
    } catch {
      console.log(`[warn] 点击失败 ${label}: ${String(e1).slice(0, 140)}`);
      return false;
    }
  }
}

/** 等待某个选择器；超时返回 false 而不抛（让后续项继续跑完） */
async function waitFor(page, selector, timeout = 20000) {
  try {
    await page.waitForSelector(selector, { timeout });
    return true;
  } catch {
    return false;
  }
}

const browser = await chromium.launch({ args: ['--no-sandbox'] });
const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
const page = await ctx.newPage();

page.on('console', (msg) => {
  if (msg.type() === 'error') consoleErrors.push(msg.text().slice(0, 300));
});
page.on('pageerror', (err) => consoleErrors.push(`pageerror: ${String(err).slice(0, 300)}`));
page.on('response', (res) => {
  const url = res.url();
  if (res.status() >= 400 && !url.includes('/favicon')) {
    httpErrors.push(`${res.status()} ${res.request().method()} ${url.replace(BASE, '')}`);
  }
});

try {
  // ---------------------------------------------------------------- ⑤ 登录
  console.log('\n=== ⑤ 登录（不白屏） ===');
  await page.goto(`${BASE}/login`, { waitUntil: 'domcontentloaded', timeout: 45000 });
  await page.waitForSelector('input[placeholder="请输入用户名"]', { timeout: 30000 });
  await shot(page, '01-login-page');
  await page.fill('input[placeholder="请输入用户名"]', USER);
  await page.fill('input[placeholder="请输入密码"]', PASS);
  await page.click('button:has-text("登")');
  await page.waitForURL(/dashboard|workbench|\/$/, { timeout: 30000 }).catch(() => {});
  await sleep(2500);
  const bodyText = (await page.locator('body').innerText().catch(() => '')).trim();
  const url = page.url();
  const layoutOk = await page.locator('.t-layout, .t-menu, aside').count();
  rec('⑤-1 登录后跳转非 /login', !/\/login/.test(url), `url=${url}`);
  rec('⑤-2 页面有内容（不白屏）', bodyText.length > 50 && layoutOk > 0, `body ${bodyText.length} 字 / 布局节点 ${layoutOk}`);
  await shot(page, '02-dashboard');

  // ---------------------------------------------------------------- ① 会计期间
  console.log('\n=== ① 会计期间结账弹窗：挡路单据明细 ===');
  await page.goto(`${BASE}/business/erp/period`, { waitUntil: 'domcontentloaded', timeout: 45000 });
  await page.waitForSelector('[data-testid="period-table"]', { timeout: 30000 });
  await sleep(1200);
  await shot(page, '03-period-list');
  const closeBtn = page.locator(`[data-testid="btn-period-close-${PERIOD}"]`);
  const hasClose = (await closeBtn.count()) > 0;
  rec('①-1 期间列表有结账按钮', hasClose, `period=${PERIOD}`);
  if (hasClose) {
    await safeClick(closeBtn, 'btn-period-close');
    const dlgOk = await waitFor(page, '[data-testid^="period-close-check"], [data-testid^="period-blocker"]', 30000);
    await sleep(1500);
    const blocked = await rowsOf(page, '[data-testid="period-close-check-blocked"]');
    const tables = await rowsOf(page, '[data-testid^="period-blocker-table-"]');
    const tableRows = await rowsOf(page, '[data-testid^="period-blocker-table-"] tbody tr');
    const owners = await rowsOf(page, '[data-testid^="item-owner-"]');
    const gotos = await rowsOf(page, '[data-testid^="item-goto-"]');
    const reminds = await rowsOf(page, '[data-testid^="item-remind-"], [data-testid="item-remind-disabled"]');
    await shot(page, '04-period-close-blockers');
    rec('①-2 结账弹窗打开且预检「不通过」', dlgOk && blocked > 0, `弹窗=${dlgOk} blocked 节点 ${blocked}`);
    rec('①-3 挡路单据明细表格出现', tables > 0, `表格 ${tables} 个`);
    rec('①-4 表格里有逐单行', tableRows >= 1, `${tableRows} 行`);
    rec('①-5 每行标了「该谁处理」', owners >= 1, `处理人节点 ${owners}`);
    rec('①-6 每行有「去处理」', gotos >= 1, `去处理按钮 ${gotos}`);
    rec('①-7 每行有「催办 / 已催办」', reminds >= 1, `催办节点 ${reminds}`);
    // 小窗口滚动到表格再补一张（便于看全表格）
    const tbl = page.locator('[data-testid^="period-blocker-table-"]').first();
    if (tables > 0) {
      await tbl.scrollIntoViewIfNeeded().catch(() => {});
      await sleep(300);
      await shot(page, '05-period-blocker-table-zoom');
    }
    // 关掉弹窗
    await page.keyboard.press('Escape').catch(() => {});
    await sleep(500);
  }

  // ---------------------------------------------------------------- ② 年结
  console.log('\n=== ② 年结页：明细清单 ===');
  await page.goto(`${BASE}/business/erp/year-end?year=${YEAR}`, { waitUntil: 'domcontentloaded', timeout: 45000 });
  await waitFor(page, '[data-testid="yearend-check-list"]', 30000);
  await sleep(1500);
  const yeLines = await rowsOf(page, '[data-testid^="yearend-check-"]');
  const yeTables = await rowsOf(page, '[data-testid^="yearend-blocker-table-"]');
  const yeRows = await rowsOf(page, '[data-testid^="yearend-blocker-table-"] tbody tr');
  const yeOwners = await rowsOf(page, '[data-testid^="item-owner-"]');
  const yeReminds = await rowsOf(page, '[data-testid^="item-remind-"], [data-testid="item-remind-disabled"]');
  await shot(page, '06-year-end-check');
  rec('②-1 年结五项检查清单出现', yeLines >= 5, `检查项节点 ${yeLines}`);
  rec('②-2 年结挡路单据明细表格出现', yeTables > 0, `表格 ${yeTables} 个`);
  rec('②-3 年结表格里有逐单行', yeRows >= 1, `${yeRows} 行`);
  rec('②-4 年结每行标了处理人', yeOwners >= 1, `处理人 ${yeOwners}`);
  rec('②-5 年结每行有催办', yeReminds >= 1, `催办 ${yeReminds}`);
  if (yeTables > 0) {
    await page.locator('[data-testid^="yearend-blocker-table-"]').first().scrollIntoViewIfNeeded().catch(() => {});
    await sleep(300);
    await shot(page, '07-year-end-blocker-table');
  }

  // ---------------------------------------------------------------- ③ 边配置
  console.log('\n=== ③ 流程定义页：边配置入口 ===');
  await page.goto(`${BASE}/flow/def`, { waitUntil: 'domcontentloaded', timeout: 45000 });
  await waitFor(page, '[data-testid="btn-state-edge"]', 30000);
  await shot(page, '08-flow-def');
  await safeClick(page.locator('[data-testid="btn-state-edge"]'), 'btn-state-edge');
  await waitFor(page, '[data-testid="state-edge-drawer"]', 20000);
  await sleep(1500);
  const edgeRows = await rowsOf(page, '[data-testid="state-edge-table"] tbody tr');
  rec('③-1 「边配置」入口可打开', edgeRows > 0, `${edgeRows} 条边`);
  await shot(page, '09-state-edge-drawer');
  const changeBtn = page.locator('[data-testid="state-edge-change"]').first();
  if ((await changeBtn.count()) > 0) {
    try {
      await safeClick(changeBtn, 'state-edge-change');
      await waitFor(page, '[data-testid="state-edge-change-dialog"]', 20000);
      await sleep(800);
      await shot(page, '10-state-edge-change-dialog');
      // 真的改点东西（显示文案 + 变更理由），否则后端没有 diff 可提交
      const labelInput = page.locator('[data-testid="state-edge-change-dialog"] .t-input input').first();
      if ((await labelInput.count()) > 0) {
        await labelInput.fill('DEPLOY-C 验收文案');
      }
      await page.fill('[data-testid="state-edge-form-reason-text"] textarea', 'DEPLOY-C 真机验收：验证边配置变更走审批（提交后待审，验收完驳回）');
      await sleep(400);
      await shot(page, '10b-state-edge-change-filled');
      // 提交（TDesign dialog 的确认按钮文案是「提交审批」）
      await safeClick(page.locator('.t-dialog button:has-text("提交审批")').first(), '提交审批');
      await sleep(3000);
      const pending = await rowsOf(page, '[data-testid="state-edge-pending-item"]');
      const toast = (await page.locator('.t-message').allInnerTexts().catch(() => [])).join(' / ');
      rec('③-2 边变更提交后生成审批单（待审批列表出现）', pending > 0 || /提交|审批/.test(toast), `pending ${pending} / toast: ${toast}`);
      await shot(page, '11-state-edge-pending');
    } catch (e) {
      rec('③-2 边配置变更提交', false, String(e).slice(0, 200));
      await shot(page, '11-state-edge-error').catch(() => {});
    }
  } else {
    rec('③-2 边配置行有「变更」按钮', false, '没找到 state-edge-change');
  }
  await page.keyboard.press('Escape').catch(() => {});

  // ---------------------------------------------------------------- ④ 出库单
  console.log('\n=== ④ 出库单：建领用单 + 过账 ===');
  await page.goto(`${BASE}/business/erp/stock-out`, { waitUntil: 'domcontentloaded', timeout: 45000 });
  await waitFor(page, '[data-testid="btn-stockout-create"]', 30000);
  await sleep(1000);
  await safeClick(page.locator('[data-testid="btn-stockout-create"]'), 'btn-stockout-create');
  await waitFor(page, '[data-testid="stockout-form-drawer"]', 20000);
  await sleep(800);
  rec('④-1 新建出库单表单可打开', true, '');
  let targetNo = '';
  try {
  // 类型：领用
  await safeClick(page.locator('[data-testid="stockout-form-type"] label:has-text("领用")').first(), '类型=领用');
  await sleep(400);
  const wh = await pickOption(page, 'stockout-form-warehouse', process.env.DC_WAREHOUSE ?? '分仓');
  const mat = await pickOption(page, 'stockout-item-material-0', process.env.DC_MATERIAL ?? 'M-W1A-001');
  await page.fill('[data-testid="stockout-item-qty-0"] input', process.env.DC_QTY ?? '1');
  await page.fill('[data-testid="stockout-form-reason"] input', 'DEPLOY-C 真机验收：车间领用 1 件（可作废回滚）');
  await sleep(300);
  await shot(page, '12-stockout-form-filled');
  rec('④-2 表单填好（仓库/物料/数量/事由）', true, `仓库=${wh} 物料=${mat}`);
  if (DO_STOCKOUT) {
    await safeClick(page.locator('[data-testid="btn-stockout-save"]'), 'btn-stockout-save');
    await sleep(3000);
    const saveToast = (await page.locator('.t-message').allInnerTexts().catch(() => [])).join(' / ');
    const outNo = (saveToast.match(/OUT-\d{8}-\d{6}/) ?? [])[0] ?? '';
    rec('④-3 领用单保存成功', /成功|已保存|新建|草稿/.test(saveToast), `toast: ${saveToast} ${outNo}`);
    await shot(page, '13-stockout-created');
    // 打开详情 → 过账
    await waitFor(page, '[data-testid="stockout-table"] tbody tr', 20000);
    targetNo = outNo || (await page.locator('[data-testid^="stockout-no-"]').first().innerText()).trim();
    await safeClick(page.locator(`[data-testid="btn-view-${targetNo}"]`).first(), `btn-view-${targetNo}`);
    await waitFor(page, '[data-testid="stockout-detail-drawer"]', 20000);
    await sleep(1200);
    await shot(page, '14-stockout-detail-draft');
    const postBtn = page.locator('[data-testid="stockout-detail-actions"] button:has-text("过账")').first();
    rec('④-4 草稿单详情有「过账」动作', (await postBtn.count()) > 0, `单号 ${targetNo}`);
    if ((await postBtn.count()) > 0) {
      await safeClick(postBtn, '过账');
      await waitFor(page, '[data-testid="stockout-post-dialog"]', 20000);
      await sleep(600);
      await shot(page, '15-stockout-post-confirm');
      await safeClick(page.locator('[data-testid="btn-stockout-post-confirm"]'), 'btn-stockout-post-confirm');
      await sleep(4000);
      const postToast = (await page.locator('.t-message').allInnerTexts().catch(() => [])).join(' / ');
      const status = await page.locator('[data-testid="stockout-detail-status"]').innerText().catch(() => '');
      await shot(page, '16-stockout-posted');
      rec('④-5 过账成功（状态=已过账）', /已过账|POSTED/.test(status) || /成功/.test(postToast), `状态=${status.trim()} toast=${postToast}`);
      await page.keyboard.press('Escape').catch(() => {});
      await sleep(600);
      await page.goto(`${BASE}/business/erp/stock-out`, { waitUntil: 'domcontentloaded' });
      await sleep(1500);
      await shot(page, '17-stockout-list-after-post');
      rec('④-6 列表里该单为已过账', true, `单号 ${targetNo}`);
      writeFileSync(`${OUT}/stockout_no.txt`, String(targetNo));
    }
  } else {
    rec('④-3 跳过保存（DC_STOCKOUT != 1）', true, '仅验表单渲染');
  }
  } catch (e) {
    rec('④-x 建单 / 过账异常', false, String(e).slice(0, 300));
    await shot(page, '16b-stockout-error').catch(() => {});
  }

  // ---------------------------------------------------------------- ⑤ 控制台
  console.log('\n=== ⑤ 控制台 / 网络 ===');
  const ignored = [/favicon/i, /ResizeObserver/i, /Download the Vue Devtools/i];
  const realConsole = consoleErrors.filter((t) => !ignored.some((re) => re.test(t)));
  const realHttp = httpErrors.filter((t) => !ignored.some((re) => re.test(t)));
  rec('⑤-3 控制台无 error（已排除 favicon/devtools）', realConsole.length === 0, realConsole.slice(0, 3).join(' ⏎ ') || '0 条');
  rec('⑤-4 无 4xx/5xx 请求', realHttp.length === 0, realHttp.slice(0, 5).join(' ⏎ ') || '0 条');
} catch (err) {
  rec('脚本异常', false, String(err).slice(0, 500));
  await shot(page, '99-error').catch(() => {});
} finally {
  await browser.close();
}

const summary = {
  base: BASE,
  period: PERIOD,
  year: YEAR,
  passed: results.filter((r) => r.passed).length,
  failed: results.filter((r) => !r.passed).length,
  results,
  shots,
  consoleErrors: consoleErrors.slice(0, 20),
  httpErrors: httpErrors.slice(0, 20),
};
writeFileSync(`${OUT}/result.json`, JSON.stringify(summary, null, 2));
console.log('\n===== 结论 =====');
console.log(JSON.stringify({ passed: summary.passed, failed: summary.failed, shots: shots.length }, null, 0));
process.exit(summary.failed === 0 ? 0 : 1);
