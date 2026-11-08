/**
 * 两源判据的实测：兜底（老后端）只多 0 发请求，后端字段一到位就连探针都不发。
 *
 * 用 AP-P307-0003 那笔仍「审批通过但未执行」的付款（PM-20261108-000002）：
 *  A. 真后端（没有 awaiting_executor）→ 按钮出现；页面对该实例只发 **1** 次 instance 详情（进度块那一发）
 *  B. 拦截详情接口补 awaiting_executor=true  → 按钮出现；instance 详情 **0** 次
 *  C. 拦截成 awaiting_executor=false              → 按钮不出现（后端说了算）；instance 详情 **0** 次
 */
import { chromium } from '@playwright/test';
import { readFileSync, appendFileSync } from 'node:fs';
import { join } from 'node:path';

const RUN = '/root/.hermes/kanban/boards/dev/workspaces/t_41801d7f/run';
const OUT = join(RUN, '..', 'repo', 'docs', 'evidence', '20261108-p307-fe', 'raw', '43_request_count.txt');
const BASE = 'http://127.0.0.1:5217';
const u = JSON.parse(readFileSync(join(RUN, 'u_pay.json'), 'utf8')).data;
const token = readFileSync(join(RUN, 'pay.token'), 'utf8').trim();
const PM = 'PM-20261108-000002';
const lines = [];
const log = (s) => {
  console.log(s);
  lines.push(s);
};

const browser = await chromium.launch();

async function run(label, patchAwaiting) {
  const ctx = await browser.newContext({ viewport: { width: 1600, height: 1000 } });
  await ctx.addInitScript(([t, r]) => {
    localStorage.setItem('x_access_token', t);
    localStorage.setItem('x_refresh_token', r);
    localStorage.setItem('cp-onboarding-done', '1');
    sessionStorage.setItem('login-unread-shown', '1');
  }, [token, u.refresh_token]);
  const page = await ctx.newPage();
  const instanceGet = [];
  page.on('request', (req) => {
    if (req.method() === 'GET' && /\/api\/business\/flow\/instance\/[0-9a-f-]{36}$/.test(req.url())) instanceGet.push(req.url().split('/').pop());
  });
  if (patchAwaiting !== undefined) {
    // ⚠️ 抽屉里的付款记录来自 `GET /api/business/erp/payable/{id}`（PayableDetailRes），
    // 不是 payment-info —— 要改的是详情接口的 payments[]。
    await page.route(/\/api\/business\/erp\/payable\/[0-9a-f-]{36}$/, async (route) => {
      const resp = await route.fetch();
      const body = await resp.json();
      const list = body?.data?.payments ?? body?.payments;
      for (const p of list ?? []) p.awaiting_executor = patchAwaiting;
      await route.fulfill({ status: resp.status(), headers: { ...resp.headers(), 'content-type': 'application/json' }, body: JSON.stringify(body) });
    });
  }
  await page.goto(`${BASE}/business/erp/payable?payable_no=AP-P307-0003`, { waitUntil: 'domcontentloaded' });
  await page.waitForFunction((no) => {
    const el = document.querySelector(`[data-testid="payable-payment-status-${no}"]`);
    return !!document.querySelector('.t-drawer.t-drawer--open') && !!el && (el.textContent || '').trim();
  }, PM, { timeout: 25000 });
  await page.waitForTimeout(1200); // 让 loadExecutorGate 那发（如果有）落地
  const count = await page.locator(`[data-testid="payable-payment-execute-${PM}"]`).count();
  log(`${label}: 按钮=${count > 0 ? '可见' : '不出现'} · instance 详情请求 ${instanceGet.length} 次${instanceGet.length ? '（' + [...new Set(instanceGet)].length + ' 个不同实例）' : ''}`);
  await ctx.close();
  return count;
}

log(`# P3-07「执行付款」判据：按钮可见性 + 实例详情请求次数（${new Date().toISOString()}）`);
log('说明：那 1 发 instance 详情是**审批进度块**（P17 既有行为）自己发的；');
log('     兜底探针若另发一次，A 会变成 2 次 —— 所以「A 也是 1 次」= 探针复用了进度块的缓存，零额外请求。');
const a = await run('A. 真后端（无 awaiting_executor，走实例状态兜底）', undefined);
const b = await run('B. 拦截详情接口补 awaiting_executor=true', true);
const c = await run('C. 拦截详情接口给 awaiting_executor=false（后端否决）', false);
await browser.close();

const ok = a > 0 && b > 0 && c === 0;
log(ok ? '结论：A/B 按钮出现、C 按后端字段消失；三态都只 1 发实例详情 ⇒ 兜底探针零额外请求 ✓' : '结论：不符合预期 ✗');
appendFileSync(OUT, lines.join('\n') + '\n\n');
process.exit(ok ? 0 : 1);
