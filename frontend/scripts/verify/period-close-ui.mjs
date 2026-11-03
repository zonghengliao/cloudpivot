/**
 * PERIOD-CLOSE 真机验证脚本（结账检查清单：列明细 / 标处理人 / 一键催办 / 直达单据）。
 *
 * 用法（前端 `pnpm dev` 与后端都已起，演示数据见 `docs/evidence/20261103-period-close/`）：
 *
 * ```bash
 * cd frontend
 * PC_PERIOD=2036-03 node scripts/verify/period-close-ui.mjs 1   # 打开弹窗 / 单张催办 / 全部催办 / 去处理
 * # …在库里把三张挡路单据处理掉（或从页面处理）…
 * PC_PERIOD=2036-03 node scripts/verify/period-close-ui.mjs 2   # 重新预检：阻断项清空、确认按钮可点
 * ```
 *
 * 产物：`$PC_OUT`（默认 `/root/pc-evidence/shots`）下的 png 截图 + stdout 的 JSON 结论。
 *
 * 为什么不用 `pnpm test:e2e`：那份 playwright 配置带**隔离实例硬判据**
 * （`e2e/target-guard.mjs`，默认只允许 5189 隔离端口），本卡验证的是本机 dev 实例（5173 + 8023），
 * 用一个独立脚本比给 target-guard 开口子安全（那条判据挡住过「e2e 打生产库」的事故）。
 */
import { chromium } from '@playwright/test';
import { mkdirSync, readFileSync } from 'node:fs';

const BASE = process.env.PC_BASE ?? 'http://127.0.0.1:5173';
const OUT = process.env.PC_OUT ?? '/root/pc-evidence/shots';
const PERIOD = process.env.PC_PERIOD ?? '2036-03';
const USER = process.env.PC_USER ?? 'admin';
const PASS = process.env.PC_PASS ?? 'Admin@123456';
const PHASE = (process.argv[2] ?? '1').trim();

mkdirSync(OUT, { recursive: true });

/** 等一个 `data-testid` 出现 */
async function waitTestId(page, id, timeout = 20000) {
  await page.waitForSelector(`[data-testid="${id}"]`, { timeout });
}

const result = { phase: PHASE, period: PERIOD, steps: [], shots: [] };

async function shot(page, name) {
  const path = `${OUT}/${name}.png`;
  await page.screenshot({ path, fullPage: false });
  result.shots.push(path);
  console.log(`[shot] ${path}`);
}

/** 读 toast 文案（TDesign MessagePlugin 渲染成 `.t-message`）
 *
 * 轮询 3 秒收集**所有出现过**的文案：连续两次操作时旧 toast 还没消失、
 * 新 toast 又可能复用同一个容器（只取 `.last()` / `allInnerTexts()` 一次
 * 实测会漏掉后一条 —— 单张催办后紧接着「全部催办」，只读到「已催办 1 张」）。 */
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
  // 优先用「已签发的令牌」注入 localStorage（`x_access_token` / `x_refresh_token`），
  // 避开后端按「账号+IP」的登录限流（10 次 / 60 秒，见 shared/constants.rs）——
  // 一轮验证要开 3 次浏览器，走 UI 登录会把自己限流成 429。
  // 没有令牌文件时才退回去真登录（带 3 次退避重试）。
  const tokenFile = process.env.PC_TOKEN_FILE ?? '/root/pc-evidence/token.txt';
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

  // ---------- 打开会计期间页 ----------
  await page.goto(`${BASE}/business/erp/period`);
  await page.waitForSelector('[data-testid="period-table"]', { timeout: 30000 });
  await waitTestId(page, `btn-period-close-${PERIOD}`);
  result.steps.push({ step: 'period-page', ok: true });

  // ---------- 打开结账弹窗 ----------
  await page.click(`[data-testid="btn-period-close-${PERIOD}"]`);
  // 预检结果有两种：还能结（`-ok`）/ 被挡（`-blocked`）。先等面板出来再按阶段断言。
  await page.waitForSelector('[data-testid="period-close-dialog"] .check-panel', { timeout: 20000 });
  await page.waitForTimeout(600);
  // phase 3 只看消息中心（收件人视角），不需要结账仍处于「被挡」状态：
  // 演示顺序里三张单可能已经被处理掉了，所以这里不给 phase 3 加「必须有阻断项」的前置。
  if (PHASE === '1') {
    await waitTestId(page, 'period-close-check-blocked');
    await page.waitForSelector('[data-testid="period-close-dialog"] tbody tr', { timeout: 20000 });
    await page.waitForTimeout(400);
  }

  const dialogText = await page.locator('[data-testid="period-close-dialog"]').innerText();
  const rows = await page.locator('[data-testid="period-close-dialog"] tbody tr').count();
  const confirmDisabled = await page
    .locator('[data-testid="period-close-dialog"] .t-dialog__footer .t-button--theme-danger')
    .first()
    .isDisabled()
    .catch(() => null);

  if (PHASE === '1') {
    await shot(page, '01-close-dialog-blocked');
    result.steps.push({
      step: 'open-close-dialog',
      ok: rows >= 3,
      rows,
      confirmDisabled,
      hasVoucher: dialogText.includes('PC-V-203603-001'),
      hasStockIn: dialogText.includes('PC-IN-203603-001'),
      hasPurchase: dialogText.includes('PC-PO-203603-001'),
      hasOwnerColumn: dialogText.includes('该谁处理'),
      hasAction: dialogText.includes('去过账') && dialogText.includes('去审批'),
    });

    // ---------- 单张催办（草稿凭证那一行） ----------
    const voucherRow = page.locator('[data-testid="period-close-dialog"] tbody tr', {
      hasText: 'PC-V-203603-001',
    });
    await voucherRow.locator('[data-testid^="item-remind-"]').first().click();
    const toast1 = await latestToast(page);
    await page.waitForTimeout(800);
    const remindedTag = await voucherRow.locator('[data-testid^="item-reminded-"]').count();
    await shot(page, '02-single-remind');
    result.steps.push({ step: 'remind-one', ok: toast1.length > 0, toast: toast1, remindedTag });

    // ---------- 全部催办 ----------
    const remindAll = page.locator('[data-testid="period-remind-all"]');
    const remindAllText = (await remindAll.innerText()).trim();
    await remindAll.click();
    const toast2 = await latestToast(page);
    await page.waitForTimeout(800);
    await shot(page, '03-remind-all');
    result.steps.push({ step: 'remind-all', ok: toast2.length > 0, button: remindAllText, toast: toast2 });

    // ---------- 去处理（收货单那行 → 直达收货页详情） ----------
    const stockInRow = page.locator('[data-testid="period-close-dialog"] tbody tr', {
      hasText: 'PC-IN-203603-001',
    });
    await stockInRow.locator('[data-testid^="item-goto-"]').first().click();
    await page.waitForURL(/business\/erp\/stock-in/, { timeout: 20000 });
    await page.waitForTimeout(2500);
    const stockInText = await page.locator('body').innerText();
    await shot(page, '04-goto-stock-in');
    result.steps.push({
      step: 'goto-document',
      // 目标页消费完深链后会 `router.replace` 掉 query（见 stockin 的 applyRouteQuery），
      // 所以判据是「详情抽屉里出现了这张单的单号」，不是 URL 里还留着 in_id。
      ok: stockInText.includes('PC-IN-203603-001'),
      url: page.url(),
      showsDocNo: stockInText.includes('PC-IN-203603-001'),
    });
  } else if (PHASE === '4') {
    // ---------- 走完「结账 → 反结账」整个流程（证明原有功能没退化） ----------
    await page.click(
      '[data-testid="period-close-dialog"] .t-dialog__footer .t-button--theme-danger',
    );
    await page.waitForTimeout(3000);
    const statusAfterClose = await page
      .locator(`[data-testid="period-status-${PERIOD}"]`)
      .innerText()
      .catch(() => '');
    await shot(page, '07-closed-period');
    result.steps.push({
      step: 'close-period',
      ok: statusAfterClose.includes('已结账'),
      status: statusAfterClose,
      confirmDisabled,
    });

    // 反结账（原因必填 ≥ 10 字）
    await page.click(`[data-testid="btn-period-reopen-${PERIOD}"]`);
    await page.waitForTimeout(800);
    await page.fill(
      '[data-testid="period-reopen-reason"] textarea',
      'PERIOD-CLOSE 真机验证：结账后立刻反结账，恢复可写',
    );
    await page.click('.t-dialog__footer .t-button--theme-warning');
    await page.waitForTimeout(3000);
    const statusAfterReopen = await page
      .locator(`[data-testid="period-status-${PERIOD}"]`)
      .innerText()
      .catch(() => '');
    await shot(page, '08-reopened-period');
    result.steps.push({
      step: 'reopen-period',
      ok: statusAfterReopen.includes('已开启'),
      status: statusAfterReopen,
    });
  } else if (PHASE === '3') {
    // ---------- 消息中心：催办真的变成了一条站内信（收件人视角） ----------
    await page.goto(`${BASE}/business/notice/message`);
    await page.waitForTimeout(3000);
    const text = await page.locator('body').innerText();
    await shot(page, '06-notice-message-center');
    result.steps.push({
      step: 'notice-center',
      ok: text.includes('2036-03 月结账待处理'),
      showsPeriod: text.includes('2036-03'),
      showsDocNo: text.includes('PC-IN-203603-001'),
      excerpt: text.slice(0, 600),
    });
  } else {
    // ---------- 处理完三张后重新预检 ----------
    const okAlert = await page.locator('[data-testid="period-close-check-ok"]').count();
    const blockedAlert = await page.locator('[data-testid="period-close-check-blocked"]').count();
    await shot(page, '05-close-dialog-cleared');
    result.steps.push({
      step: 'recheck-cleared',
      ok: okAlert === 1 && blockedAlert === 0 && confirmDisabled === false,
      okAlert,
      blockedAlert,
      confirmDisabled,
      rows,
      text: dialogText.slice(0, 200),
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
