// P0-FIX-4B 真机验收（仓库外一次性脚本，不进提交）
// 真后端 18087 + 真 dist（本次 build 产物）+ 真浏览器 Chromium（Playwright）
// 数据状态：隔离库 cloudpivot_4b（由 cloudpivot_test 克隆）；DRAFT/AUDITED 由 SQL 置位，
// 因为后端 P0-FIX-4A（DRAFT/AUDITED 三态 + audit/un-audit/post 接口）尚未实现。
import { chromium } from '@playwright/test';
import { execSync } from 'node:child_process';
import { readFileSync, mkdirSync, writeFileSync } from 'node:fs';

const BASE = 'http://127.0.0.1:5287';
const API = 'http://127.0.0.1:18087';
const ROOT = '/root/.hermes/profiles/frontend-developer/cache/scratch/4b';
const SHOTS = `${ROOT}/shots`;
mkdirSync(SHOTS, { recursive: true });

const { access_token: ACCESS, refresh_token: REFRESH } = JSON.parse(readFileSync(`${ROOT}/login.json`, 'utf8')).data;
const report = { base: BASE, api: API, steps: [], errors: [] };
const log = (step, data) => {
  report.steps.push({ step, ...data });
  console.log(`\n== ${step} ==\n` + JSON.stringify(data, null, 2));
};

function psql(sql) {
  const one = sql.replace(/'/g, `'\\''`);
  return execSync(`PGPASSWORD=alun psql -h 127.0.0.1 -U alun -d cloudpivot_4b -At -c '${one}'`, { encoding: 'utf8' }).trim();
}

async function api(path, init = {}) {
  const res = await fetch(API + path, {
    ...init,
    headers: { Authorization: `Bearer ${ACCESS}`, 'Content-Type': 'application/json', ...(init.headers ?? {}) },
  });
  const text = await res.text();
  let body = null;
  try {
    body = JSON.parse(text);
  } catch {
    body = text.slice(0, 300);
  }
  return { status: res.status, body };
}

const browser = await chromium.launch({ args: ['--no-sandbox'] });
const context = await browser.newContext({ viewport: { width: 2048, height: 1150 }, deviceScaleFactor: 1 });
await context.addInitScript(
  ([access, refresh]) => {
    localStorage.setItem('x_access_token', access);
    localStorage.setItem('x_refresh_token', refresh);
    localStorage.setItem('cp-onboarding-done', '1');
    sessionStorage.setItem('login-unread-shown', '1');
  },
  [ACCESS, REFRESH],
);
const page = await context.newPage();
const pageErrors = [];
page.on('pageerror', (error) => pageErrors.push(String(error)));
page.on('console', (msg) => {
  if (msg.type() === 'error') pageErrors.push(`console: ${msg.text()}`);
});

const search = async (no) => {
  const input = page.locator('[data-testid="voucher-filter-no"] input');
  await input.click();
  await input.fill('');
  await input.fill(no);
  await page.getByTestId('btn-voucher-search').click();
  await page.waitForTimeout(900);
};

const rowProbe = (no) =>
  page.evaluate((vno) => {
    const tag = document.querySelector(`[data-testid="voucher-status-${vno}"]`);
    const buttons = [...document.querySelectorAll(`[data-testid^="btn-voucher-"]`)]
      .map((el) => el.getAttribute('data-testid'))
      .filter((id) => id.endsWith(`-${vno}`))
      .map((id) => ({ testid: id, text: document.querySelector(`[data-testid="${id}"]`)?.textContent?.trim() }));
    return {
      statusText: tag?.textContent?.trim() ?? null,
      statusClass: tag?.getAttribute('class') ?? null,
      buttons,
      rowText: document.querySelector(`[data-testid="voucher-no-${vno}"]`)?.closest('tr')?.innerText?.replace(/\s+/g, ' ').slice(0, 200) ?? null,
    };
  }, no);

try {
  // ---------- 0. 登录态 + 页面可达 ----------
  await page.goto(`${BASE}/business/erp/voucher`);
  await page.waitForSelector('[data-testid="voucher-table"]', { timeout: 30000 });
  await page.waitForTimeout(1200);
  // 登录后「未读通知」弹窗会盖住整个列表（本仓既有行为），先关掉
  const dismiss = async () => {
    for (let i = 0; i < 4; i += 1) {
      const hit = await page.evaluate(() => {
        const ctxs = [...document.querySelectorAll('.t-dialog__ctx')].filter((ctx) => {
          const rect = ctx.getBoundingClientRect();
          return getComputedStyle(ctx).display !== 'none' && rect.width > 0 && rect.height > 0;
        });
        const last = ctxs[ctxs.length - 1];
        const button = last?.querySelector('.t-dialog__footer button');
        button?.click();
        return button ? button.textContent.trim() : null;
      });
      if (!hit) return;
      await page.waitForTimeout(500);
    }
  };
  await dismiss();

  log('0-load', {
    url: page.url(),
    title: await page.title(),
    tokenPresent: await page.evaluate(() => !!localStorage.getItem('x_access_token')),
    tableRows: await page.locator('[data-testid="voucher-table"] tbody tr').count(),
  });

  // ---------- 1. 草稿：状态列 + 只有「审核」 ----------
  const DRAFT_NO = psql("select voucher_no from biz_voucher where status='DRAFT' and deleted_at is null order by voucher_no limit 1");
  await search(DRAFT_NO);
  const draft = await rowProbe(DRAFT_NO);
  await page.screenshot({ path: `${SHOTS}/01-list-draft.png`, fullPage: false });
  log('1-draft-row', { voucherNo: DRAFT_NO, ...draft });

  // ---------- 1b. 已记账：三个审核动作一个都不出现 ----------
  const POSTED_NO = psql("select voucher_no from biz_voucher where status='POSTED' and deleted_at is null order by voucher_no limit 1");
  await search(POSTED_NO);
  const posted = await rowProbe(POSTED_NO);
  await page.screenshot({ path: `${SHOTS}/01b-list-posted.png` });
  log('1b-posted-row', { voucherNo: POSTED_NO, ...posted });

  const AUDIT_NO = psql(
    "select voucher_no from biz_voucher where status='POSTED' and deleted_at is null and is_red=0 and reversed_by is null and pending_reverse_instance_id is null order by total_debit, voucher_no limit 1",
  );
  const setAudited = psql(`update biz_voucher set status='AUDITED', updated_at=now() where voucher_no='${AUDIT_NO}' returning voucher_no, status`);
  await page.getByTestId('btn-voucher-refresh').click();
  await page.waitForTimeout(400);
  await search(AUDIT_NO);
  const audited = await rowProbe(AUDIT_NO);
  await page.screenshot({ path: `${SHOTS}/02-list-audited.png` });
  log('2-audited-row', { voucherNo: AUDIT_NO, sql: setAudited, ...audited });

  // ---------- 3. 反审核弹窗：原因必填 ----------
  await page.getByTestId(`btn-voucher-view-${AUDIT_NO}`).click();
  await page.waitForSelector('[data-testid="voucher-detail"]', { timeout: 20000 });
  await page.waitForTimeout(900);
  const detailButtons = await page.evaluate(() =>
    [...document.querySelectorAll('[data-testid="voucher-detail-audit-actions"] [data-testid^="btn-voucher-detail-"]')].map((el) => ({
      testid: el.getAttribute('data-testid'),
      text: el.textContent.trim(),
      disabled: el.className.includes('t-button--disabled') || el.hasAttribute('disabled'),
    })),
  );
  await page.getByTestId('btn-voucher-detail-un-audit').click();
  await page.waitForTimeout(700);
  const confirmState = async () =>
    page.evaluate(() => {
      const dialogs = [...document.querySelectorAll('.t-dialog')].filter((d) => d.offsetParent !== null);
      const dialog = dialogs[dialogs.length - 1];
      const buttons = [...(dialog?.querySelectorAll('.t-dialog__footer button') ?? [])].map((b) => ({
        text: b.textContent.trim(),
        disabled: b.className.includes('t-button--disabled') || b.hasAttribute('disabled'),
      }));
      return { dialogText: dialog?.innerText?.replace(/\s+/g, ' ').slice(0, 200) ?? null, buttons };
    });
  const emptyState = await confirmState();
  await page.screenshot({ path: `${SHOTS}/03-unaudit-dialog-empty.png` });

  // 填原因：走原生 setter + input/change，Vue v-model 才吃
  await page.evaluate(() => {
    const area = document.querySelector('[data-testid="voucher-unaudit-reason"] textarea') ?? document.querySelector('textarea');
    const setter = Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype, 'value').set;
    setter.call(area, '科目选错，需重做（P0-FIX-4B 真机验收）');
    area.dispatchEvent(new Event('input', { bubbles: true }));
    area.dispatchEvent(new Event('change', { bubbles: true }));
  });
  await page.waitForTimeout(500);
  const filledState = await confirmState();
  await page.screenshot({ path: `${SHOTS}/03b-unaudit-dialog-filled.png` });
  log('3-unaudit-dialog', { detailButtons, emptyState, filledState });

  // 关弹窗（取消）
  await page.evaluate(() => {
    const dialogs = [...document.querySelectorAll('.t-dialog')].filter((d) => d.offsetParent !== null);
    const dialog = dialogs[dialogs.length - 1];
    const buttons = [...(dialog?.querySelectorAll('.t-dialog__footer button') ?? [])];
    const cancel = buttons.find((b) => b.textContent.trim().includes('取消'));
    cancel?.click();
  });
  await page.waitForTimeout(600);

  // 关抽屉（自己写的关闭按钮）
  const closeBtn = page.locator('[data-testid="voucher-detail"] .t-drawer__close-btn, [data-testid="btn-drawer-close"]').first();
  if (await closeBtn.count()) {
    await closeBtn.click().catch(() => undefined);
  }
  await page.mouse.click(5, 5);
  await page.waitForTimeout(600);

  // ---------- 4. 审批进度（真 submit-reverse 产生实例；已有实例则复用） ----------
  let PROG_NO = psql("select voucher_no from biz_voucher where pending_reverse_instance_id is not null and deleted_at is null order by updated_at desc limit 1");
  if (PROG_NO) {
    const reused = psql(`select pending_reverse_instance_id from biz_voucher where voucher_no='${PROG_NO}'`);
    log('4-submit-reverse', { voucherNo: PROG_NO, reuse: true, instance: reused });
  } else {
    PROG_NO = psql(
      "select voucher_no from biz_voucher where status='POSTED' and deleted_at is null and is_red=0 and reversed_by is null and pending_reverse_instance_id is null and total_debit <= 10000 order by total_debit desc, voucher_no limit 1",
    );
    const PROG_ID = psql(`select id from biz_voucher where voucher_no='${PROG_NO}'`);
    const submit = await api(`/api/business/erp/voucher/${PROG_ID}/submit-reverse`, {
      method: 'POST',
      body: JSON.stringify({ reason: 'P0-FIX-4B 真机验收：验审批进度块' }),
    });
    log('4-submit-reverse', { voucherNo: PROG_NO, status: submit.status, body: submit.body });
  }

  await page.getByTestId('btn-voucher-refresh').click();
  await page.waitForTimeout(400);
  await search(PROG_NO);
  await page.getByTestId(`btn-voucher-view-${PROG_NO}`).click();
  await page.waitForSelector('[data-testid="voucher-detail"]', { timeout: 20000 });
  await page.waitForTimeout(1500);
  const progress = page.locator('[data-testid="voucher-approval-progress"]');
  const progressVisible = await progress.count();
  if (progressVisible) {
    await progress.scrollIntoViewIfNeeded();
    await page.waitForTimeout(600);
  }
  const progressText = progressVisible
    ? (await progress.innerText()).replace(/\s+/g, ' ').slice(0, 400)
    : await page.evaluate(() => document.querySelector('[data-testid="voucher-detail"]')?.innerText?.slice(0, 600));
  await page.screenshot({ path: `${SHOTS}/04-detail-approval-progress.png` });
  log('4-approval-progress', { voucherNo: PROG_NO, count: progressVisible, text: progressText });

  // 关抽屉
  if (await closeBtn.count()) {
    await closeBtn.click().catch(() => undefined);
  }
  await page.mouse.click(5, 5);
  await page.waitForTimeout(600);

  // ---------- 5. 点「审核」→ 真机调用 → 后端 4A 未实现（404） ----------
  await search(DRAFT_NO);
  const requests = [];
  page.on('response', (res) => {
    if (res.url().includes('/voucher/')) requests.push({ url: res.url(), status: res.status() });
  });
  await page.getByTestId(`btn-voucher-audit-${DRAFT_NO}`).click();
  await page.waitForTimeout(500);
  await page.getByRole('button', { name: '确认审核', exact: true }).click().catch(() => undefined);
  await page.waitForTimeout(2000);
  const auditCall = { requests, toast: await page.evaluate(() => [...document.querySelectorAll('.t-message')].map((el) => el.innerText.trim())) };
  await page.screenshot({ path: `${SHOTS}/05-audit-endpoint-missing.png` });
  log('5-audit-call', auditCall);
} catch (error) {
  report.errors.push(String(error));
  console.error('FAILED:', error);
  await page.screenshot({ path: `${SHOTS}/99-fail.png` }).catch(() => undefined);
} finally {
  report.pageErrors = pageErrors;
  writeFileSync(`${ROOT}/report.json`, JSON.stringify(report, null, 2));
  await browser.close();
}
