// 在线新消息右上角浮层真机验收（Playwright + 真后端 8023 + 本地 dist 5290）
import { chromium } from '@playwright/test';
import fs from 'node:fs';
import path from 'node:path';

const BASE = 'http://127.0.0.1:5290';
const OUT = '/opt/cloudpivot/docs/evidence/20261103-notify-pop';
const SCRATCH = '/root/.hermes/profiles/frontend-developer/cache/scratch/np';
fs.mkdirSync(OUT, { recursive: true });

const token = fs.readFileSync(path.join(SCRATCH, 'token_admin2.txt'), 'utf8').trim();
const results = { stages: {}, createdNotices: [], streamResponses: [] };
const browser = await chromium.launch();
const ctx = await browser.newContext({ viewport: { width: 1440, height: 900 } });
await ctx.addInitScript(
  ({ token }) => {
    localStorage.setItem('x_access_token', token);
    localStorage.setItem('x_refresh_token', token);
    localStorage.setItem('cp-onboarding-done', '1');
    // 登录未读汇总弹窗另有用例，这里先标记为「已弹过」，避免它挡住浮层
    sessionStorage.setItem('login-unread-shown', '1');
  },
  { token },
);
const page = await ctx.newPage();
page.on('pageerror', (e) => (results[`pageerror_${Date.now()}`] = String(e).slice(0, 200)));
page.on('response', (r) => {
  if (r.url().includes('/notice/stream')) results.streamResponses.push(`${r.status()} ${r.url().replace(/\?.*/, '?…')}`);
});

const TOAST = '[data-testid="notice-toast"]';

/** 造一条通知（真接口，真扇出） */
const sendNotice = async (title, noticeType, level = 'INFO') => {
  const res = await page.evaluate(
    async ({ title, noticeType, level }) => {
      const accessToken = localStorage.getItem('x_access_token');
      const me = await (await fetch('/api/system/auth/me', { headers: { Authorization: `Bearer ${accessToken}` } })).json();
      const uid = me.data.id;
      const r = await fetch('/api/business/notice', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', Authorization: `Bearer ${accessToken}` },
        body: JSON.stringify({ notice_type: noticeType, level, title, content: 'NP 验收夹具，验完删', target_type: 'USER', target_ids: [uid] }),
      });
      const body = await r.json();
      return { status: r.status, noticeId: body?.data?.notice_id, receiverCount: body?.data?.receiver_count };
    },
    { title, noticeType, level },
  );
  if (res.noticeId) results.createdNotices.push(res.noticeId);
  return res;
};

/** 等浮层可见并记录耗时 */
const waitToast = async (timeout = 12000) => {
  const started = Date.now();
  await page.waitForSelector(TOAST, { state: 'visible', timeout });
  return Date.now() - started;
};

const toastHidden = async () => {
  await page.waitForSelector(TOAST, { state: 'hidden', timeout: 15000 });
};

await page.goto(`${BASE}/dashboard`);
await page.waitForSelector('.basic-layout__header', { timeout: 30000 });
await page.waitForTimeout(2500);
results.stages.precondition = {
  toastAtStart: await page.locator(TOAST).isVisible(),
  streamResponses: [...results.streamResponses],
};

try {
  // ===== A1. 单条新通知 → 右上角自动滑出 =====
  const sent1 = await sendNotice(`[NP] 采购单 PO-20261103-000011 审批通过`, 'TODO', 'SUCCESS');
  const delay1 = await waitToast();
  await page.waitForTimeout(300);
  const box = await page.locator(TOAST).boundingBox();
  results.stages.a1 = {
    sent: sent1,
    pushDelayMs: delay1,
    title: (await page.locator('[data-testid="notice-toast-title"]').innerText()).replace(/\s+/g, ' ').trim(),
    itemCount: await page.locator('[data-testid="notice-toast-item"]').count(),
    firstItemText: (await page.locator('[data-testid="notice-toast-item"]').first().innerText()).replace(/\s+/g, ' ').trim(),
    iconClass: await page.locator('[data-testid="notice-toast-item"] .t-icon').first().getAttribute('class'),
    box,
    zIndex: await page.locator(TOAST).evaluate((el) => getComputedStyle(el).zIndex),
    transition: await page.locator(TOAST).evaluate((el) => getComputedStyle(el).transitionDuration),
  };
  await page.screenshot({ path: path.join(OUT, '10-toast-online-single.png') });

  // ===== A2. 8 秒后自动收起（两个时间点） =====
  const shownAt = Date.now();
  await toastHidden();
  results.stages.a2 = {
    visibleMs: Date.now() - shownAt,
    hiddenAfter8s: !(await page.locator(TOAST).isVisible()),
  };
  await page.screenshot({ path: path.join(OUT, '11-toast-auto-closed.png') });

  // ===== A3. 悬停暂停计时 =====
  await sendNotice('[NP] 悬停暂停计时验证', 'SYSTEM');
  await waitToast();
  await page.waitForTimeout(400);
  const box2 = await page.locator(TOAST).boundingBox();
  await page.mouse.move(box2.x + box2.width / 2, box2.y + box2.height / 2);
  await page.waitForTimeout(10000);
  const stillVisible = await page.locator(TOAST).isVisible();
  await page.screenshot({ path: path.join(OUT, '12-toast-hover-pause.png') });
  await page.mouse.move(20, 700);
  await toastHidden();
  results.stages.a3 = { visibleAfter10sHovering: stillVisible, hiddenAfterLeave: !(await page.locator(TOAST).isVisible()) };

  // ===== A4. 连续 3 条 → 合并成一个浮层「新消息 3 条」 =====
  await sendNotice('[NP] 公告：关于国庆放假的通知', 'CIRCULAR', 'INFO');
  await sendNotice('[NP] 任务「月度对账」已分派给你', 'TODO', 'INFO');
  await sendNotice('[NP] 预警：应付单据 3 天内到期', 'ALERT', 'WARN');
  await waitToast();
  await page.waitForTimeout(1200);
  const icons = await page.locator('[data-testid="notice-toast-item"] .t-icon').evaluateAll((els) => els.map((el) => el.getAttribute('class')));
  results.stages.a4 = {
    toastInstances: await page.locator(TOAST).count(),
    title: (await page.locator('[data-testid="notice-toast-title"]').innerText()).replace(/\s+/g, ' ').trim(),
    itemCount: await page.locator('[data-testid="notice-toast-item"]').count(),
    itemTitles: (await page.locator('[data-testid="notice-toast-item"]').allInnerTexts()).map((t) => t.split('\n')[0]),
    icons,
  };
  await page.screenshot({ path: path.join(OUT, '13-toast-merged-3.png') });

  // ===== A5. 点条目 → 跳消息中心并带 focus =====
  await page.locator('[data-testid="notice-toast-item"]').first().click();
  await page.waitForURL(/\/business\/notice\/message/, { timeout: 20000 });
  await page.waitForTimeout(600);
  const url = new URL(page.url());
  results.stages.a5 = {
    path: url.pathname,
    focus: url.searchParams.get('focus'),
    toastHidden: !(await page.locator(TOAST).isVisible()),
  };
  await page.screenshot({ path: path.join(OUT, '14-toast-click-item-focus.png') });

  // ===== A6. 点「查看全部」→ 跳消息中心（不带 focus） =====
  await page.goto(`${BASE}/dashboard`);
  await page.waitForTimeout(1500);
  await sendNotice('[NP] 查看全部入口验证', 'SYSTEM');
  await waitToast();
  await page.locator('[data-testid="notice-toast-all"]').click();
  await page.waitForURL(/\/business\/notice\/message/, { timeout: 20000 });
  await page.waitForTimeout(500);
  const url2 = new URL(page.url());
  results.stages.a6 = { path: url2.pathname, focus: url2.searchParams.get('focus'), toastHidden: !(await page.locator(TOAST).isVisible()) };

  // ===== A7. 点 [×] 收起 =====
  await page.goto(`${BASE}/dashboard`);
  await page.waitForTimeout(1500);
  await sendNotice('[NP] 关闭按钮验证', 'SYSTEM');
  await waitToast();
  await page.locator('[data-testid="notice-toast-close"]').click();
  await toastHidden();
  results.stages.a7 = { hidden: !(await page.locator(TOAST).isVisible()) };

  // ===== A8. 铃铛功能不受影响 =====
  await page.locator('.notice-bell__trigger').click();
  await page.waitForSelector('.notice-bell__panel', { state: 'visible', timeout: 10000 });
  await page.waitForTimeout(600);
  results.stages.a8 = {
    panelVisible: await page.locator('.notice-bell__panel').isVisible(),
    bellItems: await page.locator('.notice-bell__item').count(),
    badge: (await page.locator('.notice-bell__trigger + sup, .t-badge sup').first().innerText()).trim(),
  };
  await page.screenshot({ path: path.join(OUT, '15-bell-still-works.png') });
} catch (error) {
  results.error = String(error).slice(0, 400);
  await page.screenshot({ path: path.join(OUT, 'diag-toast-failure.png') }).catch(() => undefined);
}

results.streamResponses = [...results.streamResponses];
fs.writeFileSync(path.join(SCRATCH, 'results-toast.json'), JSON.stringify(results, null, 2));
console.log(JSON.stringify(results, null, 2));
await browser.close();
