/**
 * 临时侦察脚本（不进提交）：把 56 个页面的真实 DOM/接口画像落成 JSON，
 * 用来写死「每个页面到底长什么样」的断言基线。
 * 跑法：cd frontend && npx tsx e2e/_recon.ts
 */
import { createRequire } from 'node:module';
import { readFileSync, writeFileSync } from 'node:fs';
import { PAGES } from './_catalog.ts';

const require = createRequire(import.meta.url);
const { chromium } = require('@playwright/test');

const BASE = process.env.E2E_BASE_URL ?? 'http://172.18.10.144';
const token = process.env.E2E_TOKEN ?? readFileSync('e2e/.auth/token.txt', 'utf8').trim();
const OUT = process.env.RECON_OUT ?? '/root/.hermes/profiles/test-enginner/cache/scratch/e/profile.json';

const browser = await chromium.launch();
const ctx = await browser.newContext({
  baseURL: BASE,
  storageState: {
    cookies: [],
    origins: [{ origin: BASE, localStorage: [{ name: 'x_access_token', value: token }, { name: 'cp-onboarding-done', value: '1' }] }],
  },
  viewport: { width: 1600, height: 1000 },
  locale: 'zh-CN',
});
const page = await ctx.newPage();
let apiLog: { s: number; m: string; u: string; total?: number; rows?: number }[] = [];
let bad: string[] = [];
let errs: string[] = [];
page.on('response', async (r) => {
  const u = r.url().replace(BASE, '');
  if (!u.includes('/api/')) {
    if (r.status() >= 400) bad.push(`${r.status()} ${r.request().method()} ${u}`);
    return;
  }
  const e: { s: number; m: string; u: string; total?: number; rows?: number } = { s: r.status(), m: r.request().method(), u: u.split('?')[0] };
  if (r.status() >= 400) bad.push(`${r.status()} ${r.request().method()} ${u}`);
  if (e.m === 'GET' && r.status() === 200) {
    try {
      const j = await r.json();
      const d = j?.data;
      if (d && typeof d === 'object' && 'total' in d) {
        e.total = Number(d.total);
        e.rows = Array.isArray(d.list) ? d.list.length : undefined;
      } else if (Array.isArray(d)) {
        e.rows = d.length;
      }
    } catch {
      /* 非 JSON（导出流）忽略 */
    }
  }
  apiLog.push(e);
});
page.on('pageerror', (e) => errs.push('pageerror: ' + String(e).slice(0, 200)));
page.on('console', (m) => {
  if (m.type() === 'error') errs.push('console: ' + m.text().slice(0, 200));
});

const profile: Record<string, unknown>[] = [];
for (const spec of PAGES) {
  apiLog = [];
  bad = [];
  errs = [];
  const t0 = Date.now();
  let navStatus = 0;
  try {
    const resp = await page.goto(spec.path, { waitUntil: 'domcontentloaded', timeout: 45000 });
    navStatus = resp?.status() ?? 0;
  } catch (e) {
    navStatus = -1;
    errs.push('nav: ' + String(e).slice(0, 120));
  }
  await page.waitForSelector('.basic-layout__aside, .login-page, .t-result', { timeout: 20000 }).catch(() => {});
  await page.waitForTimeout(900);
  const dom = await page.evaluate(() => {
    const content = document.querySelector('.basic-layout__content') ?? document.body;
    const vis = (el: Element) => {
      const r = el.getBoundingClientRect();
      return r.width > 0 && r.height > 0 && getComputedStyle(el).visibility !== 'hidden';
    };
    const tables = Array.from(document.querySelectorAll('.t-table')).map((t) => {
      const trs = Array.from(t.querySelectorAll('tbody tr')).filter((r) => !r.querySelector('.t-table__empty'));
      return {
        rows: trs.length,
        ths: Array.from(t.querySelectorAll('thead th')).map((h) => (h.textContent || '').trim()).filter(Boolean).slice(0, 14),
        firstRow: (trs[0]?.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 100),
        empty: (t.querySelector('.t-table__empty')?.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 60),
        pagination: (document.querySelector('.t-pagination__total')?.textContent || '').trim(),
      };
    });
    const buttons = Array.from(content.querySelectorAll('button'))
      .filter(vis)
      .map((b) => ({
        t: (b.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 24),
        dis: b.disabled,
        icon: !!b.querySelector('.t-icon'),
        cls: b.className.replace(/t-button\s*/g, '').slice(0, 40),
      }));
    const dialogs = Array.from(document.querySelectorAll('.t-dialog, .t-drawer'))
      .filter(vis)
      .map((d) => (d.textContent || '').replace(/\s+/g, ' ').trim().slice(0, 80));
    const inputs = Array.from(content.querySelectorAll('input, textarea, .t-select, .t-date-picker'))
      .filter(vis).length;
    return {
      title: document.title,
      contentText: content.innerText.replace(/\s+/g, ' ').slice(0, 260),
      tables,
      buttons,
      dialogs,
      inputs,
      cards: content.querySelectorAll('.dashboard__entry, .board-card, .todo-card, .page-card, .help-cat, .t-card').length,
      emptyHints: (content.innerText.match(/暂无数据|暂无内容|没有数据|无数据|暂无搜索结果|空空如也/g) ?? []).slice(0, 3),
    };
  });
  profile.push({
    spec,
    navStatus,
    ms: Date.now() - t0,
    api: apiLog.filter((e) => !e.u.includes('/auth/') && !e.u.includes('/badge/')),
    bad,
    errs,
    ...dom,
  });
  console.log(
    `[${spec.key}] ${spec.path} nav=${navStatus} api=${apiLog.length} bad=${bad.length} err=${errs.length} rows=${dom.tables.map((t) => t.rows).join(',')} btn=${dom.buttons.length} dlg=${dom.dialogs.length} title=${dom.title}`,
  );
  await page.waitForTimeout(400);
}
writeFileSync(OUT, JSON.stringify(profile, null, 1));
console.log('written', OUT, profile.length);
await browser.close();
