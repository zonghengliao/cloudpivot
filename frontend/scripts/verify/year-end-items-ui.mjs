/**
 * YEAR-END 真机验证脚本（年结检查清单：列明细 / 标处理人 / 一键催办 / 去处理直达）。
 *
 * 用法（前端 `pnpm dev` 与后端都已起，演示数据见 `docs/evidence/20261103-year-end/05-demo-data.sql`）：
 *
 * ```bash
 * cd frontend
 * YE_YEAR=2040 node scripts/verify/year-end-items-ui.mjs 1   # 明细表 / 单张催办 / 全部催办 / 去处理（调拨单）
 * YE_YEAR=2040 node scripts/verify/year-end-items-ui.mjs 2   # 处理完三张后重新预检：明细消失、可年结
 * YE_YEAR=2040 node scripts/verify/year-end-items-ui.mjs 3   # 消息中心：催办真的变成站内信
 * YE_YEAR=2040 node scripts/verify/year-end-items-ui.mjs 4   # 调拨页 ?transfer_no= 深链直达详情
 * ```
 *
 * 产物：`$YE_OUT`（默认 `/root/ye-evidence/shots`）下的 png 截图 + stdout 的 JSON 结论。
 *
 * 为什么不用 `pnpm test:e2e`：那份 playwright 配置带**隔离实例硬判据**
 * （`e2e/target-guard.mjs`，默认只允许 5189 隔离端口），本卡验证的是本机 dev 实例（5173 + 8023），
 * 用一个独立脚本比给 target-guard 开口子安全。
 */
import { chromium } from '@playwright/test';
import { mkdirSync, readFileSync } from 'node:fs';

const BASE = process.env.YE_BASE ?? 'http://127.0.0.1:5173';
const OUT = process.env.YE_OUT ?? '/root/ye-evidence/shots';
const YEAR = process.env.YE_YEAR ?? '2040';
const TRANSFER_NO = process.env.YE_TRANSFER ?? 'PC-YE-TR-2040-001';
const PO_NO = process.env.YE_PO ?? 'PC-YE-PO-2040-001';
const IN_NO = process.env.YE_IN ?? 'PC-YE-IN-2040-001';
const USER = process.env.YE_USER ?? 'admin';
const PASS = process.env.YE_PASS ?? 'Admin@123456';
const PHASE = (process.argv[2] ?? '1').trim();

mkdirSync(OUT, { recursive: true });

/** 等一个 `data-testid` 出现 */
async function waitTestId(page, id, timeout = 20000) {
  await page.waitForSelector(`[data-testid="${id}"]`, { timeout });
}

const result = { phase: PHASE, year: YEAR, steps: [], shots: [] };

async function shot(page, name) {
  const path = `${OUT}/${name}.png`;
  await page.screenshot({ path, fullPage: false });
  result.shots.push(path);
  console.log(`[shot] ${path}`);
}

/**
 * 读 toast 文案（TDesign MessagePlugin 渲染成 `.t-message`）。
 *
 * 轮询 3 秒收集**所有出现过**的文案：连续两次操作时旧 toast 还没消失，
 * 新 toast 又可能复用同一个容器 —— 只取一次会漏掉后一条。
 */
async function latestToast(page) {
  const seen = [];
  for (let i = 0; i < 15; i += 1) {
    const texts = await page.locator('.t-message').allInnerTexts().catch(() => []);
    for (const raw of texts) {
      const text = raw.trim();
      if (text && !seen.includes(text)) {
        seen.push(text);
      }
    }
    await page.waitForTimeout(200);
  }
  return seen.join(' | ');
}

const browser = await chromium.launch();
const context = await browser.newContext({ viewport: { width: 1600, height: 1000 } });
const page = await context.newPage();

try {
  // ---------- 登录 ----------
  //
  // 优先用「已签发的令牌」注入 localStorage（`x_access_token`），
  // 避开后端按「账号+IP」的登录限流（10 次 / 60 秒）——
  // 一轮验证要开 4 次浏览器，走 UI 登录会把自己限流成 429。
  const tokenFile = process.env.YE_TOKEN_FILE ?? '/root/ye-evidence/token.txt';
  let token = '';
  try {
    token = readFileSync(tokenFile, 'utf8').trim();
  } catch {
    token = '';
  }
  if (token) {
    await page.goto(`${BASE}/login`);
    await page.evaluate((value) => {
      localStorage.setItem('x_access_token', value);
    }, token);
    await page.goto(`${BASE}/dashboard`);
    await page.waitForURL(/\/(dashboard|change-password)/, { timeout: 30000 });
    result.steps.push({ step: 'login', ok: true, via: 'token', url: page.url() });
  } else {
    let lastLoginError = '';
    for (let attempt = 1; attempt <= 3; attempt += 1) {
      await page.goto(`${BASE}/login`);
      await page.waitForSelector("input[placeholder='请输入用户名']");
      await page.fill("input[placeholder='请输入用户名']", USER);
      await page.fill("input[placeholder='请输入密码']", PASS);
      await page.click('button.t-button--theme-primary');
      try {
        await page.waitForURL(/\/(dashboard|change-password)/, { timeout: 30000 });
        break;
      } catch {
        lastLoginError =
          (await page.locator('.t-alert').first().innerText().catch(() => '')) || '未跳转到工作台';
        await page.waitForTimeout(2000 * attempt);
      }
    }
    if (!/\/(dashboard|change-password)/.test(page.url())) {
      throw new Error(`登录失败（已重试 3 次）：${lastLoginError}`);
    }
    result.steps.push({ step: 'login', ok: true, via: 'form', url: page.url() });
  }

  // ---------- 打开年结页（?year= 深链预选年度）----------
  await page.goto(`${BASE}/business/erp/year-end?year=${YEAR}`);
  await page.waitForSelector('[data-testid="yearend-check-list"]', { timeout: 30000 });
  await page.waitForTimeout(1200);
  result.steps.push({ step: 'year-end-page', ok: true, url: page.url() });

  if (PHASE === '1') {
    // ---------- 明细表 ----------
    await waitTestId(page, 'yearend-blocker-DOC_UNFINISHED');
    await page.waitForTimeout(600);
    const table = page.locator('[data-testid="yearend-blocker-table-DOC_UNFINISHED"]');
    const rows = await table.locator('tbody tr').count();
    const text = await page.locator('body').innerText();
    await shot(page, '01-year-end-items-table');
    result.steps.push({
      step: 'items-table',
      ok: rows >= 3,
      rows,
      hasPo: text.includes(PO_NO),
      hasStockIn: text.includes(IN_NO),
      hasTransfer: text.includes(TRANSFER_NO),
      hasOwnerColumn: text.includes('该谁处理'),
      hasPendingOwner: text.includes('待分配'),
      hasAction: text.includes('去过账') && text.includes('去审批'),
      hint: text.includes('张挡路单据'),
    });

    // ---------- 单张催办（采购单那一行）----------
    const poRow = table.locator('tbody tr', { hasText: PO_NO });
    await poRow.locator('[data-testid^="item-remind-"]').first().click();
    const toast1 = await latestToast(page);
    await page.waitForTimeout(800);
    const remindedTag = await poRow.locator('[data-testid^="item-reminded-"]').count();
    await shot(page, '02-year-end-remind-one');
    result.steps.push({ step: 'remind-one', ok: toast1.includes('已催办'), toast: toast1, remindedTag });

    // ---------- 全部催办 ----------
    const remindAll = page.locator('[data-testid="yearend-remind-all"]');
    const remindAllText = (await remindAll.innerText()).trim();
    await remindAll.click();
    const toast2 = await latestToast(page);
    await page.waitForTimeout(800);
    await shot(page, '03-year-end-remind-all');
    result.steps.push({
      step: 'remind-all',
      ok: toast2.includes('已催办'),
      button: remindAllText,
      toast: toast2,
    });

    // ---------- 去处理：调拨单那一行 → 直达调拨页详情（本卡补的深链）----------
    const trRow = table.locator('tbody tr', { hasText: TRANSFER_NO });
    await trRow.locator('[data-testid^="item-goto-"]').first().click();
    await page.waitForURL(/business\/erp\/transfer/, { timeout: 20000 });
    await page.waitForTimeout(2500);
    const trText = await page.locator('body').innerText();
    await shot(page, '04-goto-transfer-detail');
    result.steps.push({
      step: 'goto-transfer',
      // 目标页消费完深链后会 `router.replace` 掉 query，判据是「详情抽屉里出现了这张单的单号」
      ok: trText.includes(TRANSFER_NO),
      url: page.url(),
      showsDocNo: trText.includes(TRANSFER_NO),
    });
  } else if (PHASE === '2') {
    // ---------- 三张都处理掉后重新预检：明细消失、年度结转可点 ----------
    const itemTables = await page.locator('[data-testid^="yearend-blocker-table-"]').count();
    const body = await page.locator('body').innerText();
    const okLines = await page.locator('[data-testid="yearend-check-DOC_UNFINISHED"]').innerText();
    const closeBtn = page.locator('[data-testid="btn-yearend-close"]');
    const closeDisabled = await closeBtn.isDisabled().catch(() => null);
    await shot(page, '05-year-end-cleared');
    result.steps.push({
      step: 'recheck-cleared',
      ok: itemTables === 0 && okLines.includes('✅') && closeDisabled === false,
      itemTables,
      lineText: okLines,
      closeDisabled,
      text: body.slice(0, 300),
    });
  } else if (PHASE === '3') {
    // ---------- 消息中心：催办真的变成了一条站内信（收件人视角）----------
    await page.goto(`${BASE}/business/notice/message`);
    await page.waitForTimeout(3000);
    const text = await page.locator('body').innerText();
    await shot(page, '06-year-end-notice-center');
    result.steps.push({
      step: 'notice-center',
      ok: text.includes(`${YEAR} 年结账待处理`),
      showsYear: text.includes(`${YEAR} 年结账待处理`),
      showsDocNo: text.includes(PO_NO) || text.includes(IN_NO),
      excerpt: text.slice(0, 600),
    });
  } else if (PHASE === '4') {
    // ---------- 调拨页深链（不经过年结页，直接开链接）----------
    await page.goto(`${BASE}/business/erp/transfer?transfer_no=${encodeURIComponent(TRANSFER_NO)}`);
    await page.waitForTimeout(3000);
    const text = await page.locator('body').innerText();
    const drawer = await page.locator('[data-testid="transfer-detail-drawer"]').count();
    await shot(page, '07-transfer-deeplink');
    result.steps.push({
      step: 'transfer-deeplink',
      ok: drawer > 0 && text.includes(TRANSFER_NO),
      drawer,
      url: page.url(),
      showsDocNo: text.includes(TRANSFER_NO),
    });
  } else if (PHASE === '5') {
    // ---------- 走完「年结 → 反年结」（证明原有功能没退化、can_close 真的放行）----------
    const closeBtn = page.locator('[data-testid="btn-yearend-close"]');
    const closeDisabled = await closeBtn.isDisabled().catch(() => null);
    await closeBtn.click();
    await waitTestId(page, 'yearend-close-dialog');
    await page.fill('[data-testid="yearend-close-reason"] input', 'YEAR-END 真机验证：结转 2040 年度');
    await page.click('[data-testid="yearend-close-dialog"] .t-dialog__footer .t-button--theme-primary');
    await page.waitForTimeout(4000);
    const statusAfterClose = await page
      .locator('[data-testid="yearend-status-tag"]')
      .innerText()
      .catch(() => '');
    await shot(page, '08-year-end-closed');
    result.steps.push({
      step: 'close-year',
      ok: statusAfterClose.includes('已结转'),
      status: statusAfterClose,
      closeDisabled,
    });

    // 反年结（原因必填 ≥ 10 字）
    await page.click('[data-testid="btn-yearend-reopen"]');
    await waitTestId(page, 'yearend-reopen-dialog');
    await page.fill(
      '[data-testid="yearend-reopen-reason"] input',
      'YEAR-END 真机验证：结转后立刻反年结，恢复可重跑',
    );
    await page.click('[data-testid="yearend-reopen-dialog"] .t-dialog__footer .t-button--theme-danger');
    await page.waitForTimeout(4000);
    const statusAfterReopen = await page
      .locator('[data-testid="yearend-status-tag"]')
      .innerText()
      .catch(() => '');
    await shot(page, '09-year-end-reopened');
    result.steps.push({
      step: 'reopen-year',
      ok: statusAfterReopen.includes('未结转'),
      status: statusAfterReopen,
    });
  }
} catch (error) {
  result.error = String(error?.stack ?? error);
  await shot(page, `99-failure-${PHASE}`);
} finally {
  await browser.close();
}

console.log(JSON.stringify(result, null, 2));
process.exit(result.error ? 1 : 0);
