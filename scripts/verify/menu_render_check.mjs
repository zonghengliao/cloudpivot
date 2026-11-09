#!/usr/bin/env node
/**
 * 侧栏菜单**渲染**体检：DB 里 icon 写对了不等于界面上看得见。
 *
 * # 为什么需要它（MENU-FIX-3，卡 t_7c9a6c3b）
 *
 * `sys_menu.icon` 存的是 TDesign 图标组件名。名字不存在时 `SideMenuItem.vue` 的
 * `<t-icon :name="item.icon" />` 渲染**空节点**，侧栏那一项就没有图标；而 `icon` 非空串
 * 时连组件里的 `folder` / `app` 兜底也不会触发。于是「DB icon='shopping-cart'、界面空白」
 * 这种状态用任何纯 SQL 检查都看不出来 —— 必须真的把页面渲染出来看 DOM。
 *
 * 实测记录：`tdesign-icons-vue-next/esm/components/` 里有 `cart.js`，**没有**
 * `shopping-cart.js`；145 dev 的 60 个 distinct icon 全部存在，144 prod 只有
 * `shopping-cart` 一个不存在（V0353 修的就是它）。
 *
 * # 它做什么
 *
 * 1. 用 API 登录（`POST /api/system/auth/login`），拿 `/api/system/auth/menus` 的菜单树；
 * 2. 打开前端，按菜单树逐个**展开顶级目录**（TDesign 侧栏是手风琴，展开一个会收起别的，
 *    所以一次只验一个），逐个菜单项检查 `li` 里有没有 `svg.t-icon`；
 * 3. 任何一项**没有 svg**（也就是界面空白）→ 记为 BLANK，退出码 1；
 * 4. 打印 目录 → 子项（含图标名）的完整清单，便于人工对照 DB。
 *
 * # 用法
 *
 * ```bash
 * FE_PASS="$(ssh root@172.18.10.145 'grep -m1 ^NIK_ADMIN_PASSWORD= /etc/cloudpivot/cloudpivot-dev.env | cut -d= -f2-')" \
 *   node scripts/verify/menu_render_check.mjs
 * ```
 *
 * 环境变量：
 *   FE_BASE    被测前端地址，默认 `http://172.18.10.145`（145 测试环境）
 *   FE_USER    登录账号，默认 `admin`
 *   FE_PASS    登录口令（**必填**，不落脚本；145 取 `/etc/cloudpivot/cloudpivot-dev.env` 的 NIK_ADMIN_PASSWORD）
 *   OUT_DIR    截图目录，默认 `./menu_render_check_out`
 *
 * 退出码：0 = 没有空白图标 ｜ 1 = 有空白图标（或登录/渲染失败）
 */

import { createRequire } from 'node:module';
import { mkdirSync, writeFileSync } from 'node:fs';

// 前端依赖按 frontend/package.json 解析（本脚本只读前端 node_modules，不改它）
const require = createRequire(new URL('../../frontend/package.json', import.meta.url).pathname);
const { chromium, request } = require('@playwright/test');

const BASE = (process.env.FE_BASE ?? 'http://172.18.10.145').replace(/\/$/, '');
const USER = process.env.FE_USER ?? 'admin';
const PASS = process.env.FE_PASS ?? '';
const OUT = process.env.OUT_DIR ?? 'menu_render_check_out';

if (!PASS) {
  console.error('缺少 FE_PASS（不落脚本：145 取 /etc/cloudpivot/cloudpivot-dev.env 的 NIK_ADMIN_PASSWORD）');
  process.exit(1);
}
mkdirSync(OUT, { recursive: true });
const log = (...a) => console.log(...a);

// ---------------------------------------------------------------- ① API 侧
const api = await request.newContext({ baseURL: BASE });
const loginRes = await api.post('/api/system/auth/login', { data: { username: USER, password: PASS } });
const loginBody = await loginRes.json().catch(() => ({}));
if (loginRes.status() !== 200 || loginBody?.code !== 0) {
  console.error(`登录失败 ${loginRes.status()} ${JSON.stringify(loginBody).slice(0, 200)}`);
  process.exit(1);
}
const { access_token: at, refresh_token: rt } = loginBody.data;

const menusRes = await api.get('/api/system/auth/menus', { headers: { Authorization: `Bearer ${at}` } });
const menusBody = await menusRes.json().catch(() => ({}));
if (menusRes.status() !== 200 || menusBody?.code !== 0) {
  console.error(`取菜单失败 ${menusRes.status()} ${JSON.stringify(menusBody).slice(0, 200)}`);
  process.exit(1);
}
const tree = menusBody.data ?? [];
const topDirs = tree.filter((n) => Array.isArray(n.children) && n.children.some((c) => c.menu_type === 2));
log(`API 菜单树：顶级 ${tree.length} 个，其中可展开目录 ${topDirs.length} 个`);

// ---------------------------------------------------------------- ② 打开前端
const browser = await chromium.launch({ args: ['--no-sandbox'] });
const ctx = await browser.newContext({ viewport: { width: 1600, height: 1100 }, locale: 'zh-CN' });
const page = await ctx.newPage();
const pageErrors = [];
page.on('pageerror', (e) => pageErrors.push(String(e).slice(0, 200)));

await page.goto(`${BASE}/login`, { waitUntil: 'domcontentloaded' });
await page.evaluate(([a, r]) => {
  localStorage.setItem('x_access_token', a);
  localStorage.setItem('x_refresh_token', r);
  localStorage.setItem('cp-onboarding-done', '1'); // 新功能引导浮层会挡点击
}, [at, rt]);
// networkidle 不可用：通知流是 SSE 长连接，永远等不到
await page.goto(`${BASE}/dashboard`, { waitUntil: 'domcontentloaded' });
await page.waitForSelector('.t-menu', { timeout: 60000 });
await page.waitForTimeout(2500);

// 登录后「您有 N 件待处理」弹层会挡住侧栏点击
const ack = page.locator('.t-dialog button', { hasText: '知道了' }).first();
if (await ack.count()) {
  await ack.click().catch(() => {});
  await page.waitForTimeout(800);
}

// ---------------------------------------------------------------- ③ DOM 探针
const titleNode = (kw) => page.locator('.side-menu-item__title').filter({ hasText: new RegExp(`^${kw}$`) }).first();

async function expandGroup(kw) {
  const t = titleNode(kw);
  if ((await t.count()) === 0) return false;
  const li = t.locator('xpath=ancestor::li[contains(@class,"t-submenu")][1]');
  if (!((await li.getAttribute('class')) ?? '').includes('t-is-opened')) {
    await t.click();
    await page.waitForTimeout(700);
  }
  return true;
}

/** 侧栏里所有**可见**行：标题 / y / 层级 / 图标 svg class（无 svg = 界面空白） */
const dumpRows = () =>
  page.evaluate(() => {
    const out = [];
    for (const t of document.querySelectorAll('.side-menu-item__title')) {
      const title = (t.textContent ?? '').trim();
      const li = t.closest('li');
      if (!li) continue;
      const box = li.getBoundingClientRect();
      if (box.width === 0 || box.height === 0) continue; // 收起的分组不参与
      const svg = li.querySelector('svg.t-icon'); // 图标是 li 内直接的 svg.t-icon（不是 .t-menu__icon 里）
      let depth = 0;
      for (let el = li.parentElement; el; el = el.parentElement) {
        if (el.tagName === 'LI' && el.classList?.contains('t-submenu')) depth += 1;
      }
      out.push({ title, y: Math.round(box.y), depth, hasSvg: Boolean(svg), iconClass: svg?.getAttribute('class') ?? null });
    }
    return out.sort((a, b) => a.y - b.y);
  });

const report = [];
const blanks = [];
for (const dir of topDirs) {
  const ok = await expandGroup(dir.title);
  if (!ok) {
    report.push({ dir: dir.title, error: '侧栏里找不到该目录' });
    blanks.push({ dir: dir.title, title: '(目录本身)', reason: '目录行未找到' });
    continue;
  }
  const rows = await dumpRows();
  const idx = rows.findIndex((r) => r.title === dir.title);
  const kids = [];
  for (const r of rows.slice(idx + 1)) {
    if (r.depth <= rows[idx].depth) break; // 到了下一个同级/上级
    if (r.depth === rows[idx].depth + 1) kids.push(r);
  }
  // 目录自身也必须渲染出图标
  if (!rows[idx]?.hasSvg) blanks.push({ dir: dir.title, title: '(目录本身)', reason: '无 svg' });
  for (const k of kids) {
    if (!k.hasSvg) blanks.push({ dir: dir.title, title: k.title, reason: '无 svg（界面空白）' });
  }
  report.push({
    dir: dir.title,
    dirIcon: rows[idx]?.iconClass,
    children: kids.map((k) => `${k.title} [${k.iconClass ?? 'BLANK'}]`),
  });
  await page.screenshot({
    path: `${OUT}/dir_${String(report.length).padStart(2, '0')}.png`,
    fullPage: false,
  });
}

// ---------------------------------------------------------------- ④ 报告
log('\n=== 侧栏渲染清单（DB 名 → 渲染出的 svg class） ===');
for (const r of report) {
  if (r.error) {
    log(`⚠ ${r.dir}：${r.error}`);
    continue;
  }
  log(`\n${r.dir}  (自身图标 ${r.dirIcon ?? 'BLANK'})`);
  for (const c of r.children) log(`   ├ ${c}`);
}

log('\n=== 结论 ===');
if (blanks.length === 0) {
  log(`✅ 全部 ${report.reduce((n, r) => n + (r.children?.length ?? 0), 0)} 个菜单项都渲染出了图标（没有空白）`);
} else {
  log(`❌ 有 ${blanks.length} 处空白图标（icon 名在 tdesign-icons-vue-next 里不存在，或菜单没取到 icon）：`);
  for (const b of blanks) log(`   · [${b.dir}] ${b.title} —— ${b.reason}`);
}
if (pageErrors.length) log(`⚠ 页面 JS 报错 ${pageErrors.length} 条（前 3 条）：${pageErrors.slice(0, 3).join(' | ')}`);

writeFileSync(`${OUT}/report.json`, JSON.stringify({ base: BASE, blanks, report, pageErrors }, null, 2));
log(`\n清单/截图目录：${OUT}`);

await browser.close();
await api.dispose();
process.exit(blanks.length === 0 ? 0 : 1);
