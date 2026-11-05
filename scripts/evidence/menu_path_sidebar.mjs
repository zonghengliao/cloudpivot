#!/usr/bin/env node
/**
 * P2-2B-FIX-2（t_ff4f92eb）取证：手册里「左侧 分组 → 页面」与本实例真实菜单树是否一致。
 *
 * 用法：
 *   MANUAL_BASE=http://127.0.0.1:5180 OUT=<目录> node scripts/evidence/menu_path_sidebar.mjs
 *
 * 输出（OUT 目录）：
 *   01-menu-api-parent.txt  每个目标页面在 /api/system/auth/menus 里的父链（侧边栏数据源）
 *   menus-api.json          完整菜单树（原始响应，供人工复核）
 *   02-sidebar-dom.txt      侧边栏 DOM 层级文本（逐组展开后导出）
 *   03-sidebar-<label>.png  打开目标页面后、把所在分组逐级展开的侧边栏
 *   04-page-<label>.png     目标页面全屏（侧边栏 + 列表）
 *   crumb-<label>.txt       面包屑原文
 *
 * 只读：只做登录、拉菜单 API、切侧边栏展开、打开列表页，不点任何写库按钮。
 */
import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';
import fs from 'node:fs';
import path from 'node:path';

const BASE = process.env.MANUAL_BASE || 'http://127.0.0.1:5180';
const USER = process.env.MANUAL_USER || 'admin';
const PASS = process.env.MANUAL_PASSWORD || 'Manual@123456';
const OUT = process.env.OUT || '/opt/cloudpivot/docs/evidence/20261105-menu-path-fix/after';
fs.mkdirSync(OUT, { recursive: true });

/** 手册里写了「左侧 … → …」的页面；parent 为手册**改后**的期望分组链（用于 fail-closed 断言） */
const TARGETS = [
  { path: '/business/erp/payable', label: 'payable', name: '应付单', expect: ['采购管理'] },
  { path: '/business/erp/inventory', label: 'inventory', name: '即时库存', expect: ['仓库管理'] },
  { path: '/business/erp/transfer', label: 'transfer', name: '库存调拨', expect: ['仓库管理'] },
  { path: '/business/erp/stock-out', label: 'stock-out', name: '出库单', expect: ['采购管理', '采购订单'] },
  // 只取证、不写进手册（盘点单归组待业务口径，见卡面「不要做的事」）
  { path: '/business/erp/stock-take', label: 'stock-take', name: '盘点单', expect: null },
];

// ---------- ① 登录 ----------
const loginRes = await fetch(`${BASE}/api/system/auth/login`, {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ username: USER, password: PASS }),
});
const loginBody = await loginRes.json().catch(() => ({}));
if (loginBody.code !== 0 || !loginBody.data?.access_token) {
  console.error('登录失败', loginRes.status, JSON.stringify(loginBody).slice(0, 200));
  process.exit(2);
}
const token = loginBody.data.access_token;
const refresh = loginBody.data.refresh_token;

// ---------- ② 菜单 API（侧边栏数据源）----------
const menusRes = await fetch(`${BASE}/api/system/auth/menus`, {
  headers: { Authorization: `Bearer ${token}` },
});
const menusBody = await menusRes.json();
fs.writeFileSync(path.join(OUT, 'menus-api.json'), JSON.stringify(menusBody, null, 2));

function walk(nodes, chain = [], out = new Map()) {
  for (const n of nodes || []) {
    out.set(n.path, { node: n, chain });
    walk(n.children, [...chain, n], out);
  }
  return out;
}
const flat = walk(menusBody.data);
const chainTitles = (x) => x.chain.map((c) => c.title).join(' → ') || '（顶层）';

const apiLines = [`实例: ${BASE}  账号: ${USER}  时间: ${new Date().toISOString()}`, ''];
let apiFail = 0;
for (const t of TARGETS) {
  const hit = flat.get(t.path);
  if (!hit) {
    apiLines.push(`[API] ${t.name} ${t.path} → 未在菜单树中找到`);
    apiFail++;
    continue;
  }
  const chain = chainTitles(hit);
  const ok = t.expect ? t.expect.join(' → ') === chain : null;
  if (ok === false) apiFail++;
  apiLines.push(
    `[API] ${t.name}（${t.path}）→ 父链 ${chain}  menu_type=${hit.node.menu_type}  ` +
      (ok === null ? '（仅取证）' : ok ? 'PASS' : `FAIL 期望 ${t.expect.join(' → ')}`),
  );
}
apiLines.push('');
// 附录：两个分组当前的全部子节点，便于人工复核手册里没点到的页面
for (const gp of ['/business/erp/purchase-mgmt', '/business/erp/warehouse-mgmt', '/business/erp/finance-mgmt']) {
  const g = flat.get(gp);
  if (g) apiLines.push(`[API] 分组 ${g.node.title} 子节点 = ${(g.node.children || []).map((c) => c.title).join(' / ')}`);
}
fs.writeFileSync(path.join(OUT, '01-menu-api-parent.txt'), apiLines.join('\n') + '\n');
console.log(apiLines.join('\n'));

// ---------- ③ 浏览器：侧边栏 DOM + 截图 ----------
const browser = await chromium.launch();
const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, locale: 'zh-CN' });
await context.addInitScript(
  ([a, r]) => {
    localStorage.setItem('x_access_token', a);
    localStorage.setItem('x_refresh_token', r);
    localStorage.setItem('x_guide_done', '1');
    localStorage.setItem('cp-onboarding-done', '1');
    localStorage.setItem('cp-guide-dismissed', '1');
  },
  [token, refresh],
);
const page = await context.newPage();
page.setDefaultTimeout(20000);

async function dismissOverlays() {
  for (let i = 0; i < 5; i++) {
    let hit = false;
    for (const sel of [
      '[data-testid="login-unread-dialog"] button:has-text("知道了")',
      'button:has-text("知道了")',
      '.t-dialog__close',
      '.guide-popup .close',
    ]) {
      const el = page.locator(sel).first();
      if (await el.isVisible().catch(() => false)) {
        await el.click({ timeout: 2000 }).catch(() => {});
        hit = true;
        await page.waitForTimeout(250);
        break;
      }
    }
    if (!hit) break;
  }
}

/**
 * 展开侧边栏里的某一级目录（TDesign：`li.t-submenu > div.t-menu__item` 是开关；
 * t-menu 设了 expand-mutex，同级后点开的会收起前一个 —— 因此要先展开父级再展开子级）。
 */
async function expandGroup(title) {
  const li = page.locator('.basic-layout__aside li.t-submenu');
  const n = await li.count();
  for (let i = 0; i < n; i++) {
    const cand = li.nth(i);
    const head = cand.locator(':scope > div.t-menu__item').first();
    const txt = (await head.innerText().catch(() => '')).replace(/\s+/g, '');
    if (txt === title) {
      await head.scrollIntoViewIfNeeded().catch(() => {});
      if ((await cand.getAttribute('class'))?.includes('t-is-opened')) return true; // 已展开，别点（会收起）
      await head.click();
      await page.waitForTimeout(700);
      return true;
    }
  }
  return false;
}

async function sidebarRows() {
  return page.evaluate(() => {
    const menu = document.querySelector('.basic-layout__aside .t-menu');
    if (!menu) return [];
    const out = [];
    const clean = (t) => (t || '').replace(/\s+/g, ' ').trim();
    const walk = (ul, depth) => {
      for (const li of ul.querySelectorAll(':scope > li')) {
        if (li.classList.contains('t-submenu')) {
          const head = li.querySelector(':scope > div.t-menu__item');
          out.push({ depth, kind: 'dir ', text: clean(head?.innerText), open: li.classList.contains('t-is-opened') });
          const sub = li.querySelector(':scope > ul.t-menu__sub');
          if (sub) walk(sub, depth + 1);
        } else if (li.classList.contains('t-menu__item')) {
          out.push({ depth, kind: 'page', text: clean(li.innerText), open: null });
        }
      }
    };
    walk(menu, 0);
    return out;
  });
}

const domLines = [`实例: ${BASE}  采集时间: ${new Date().toISOString()}`, ''];
const pageResults = [];
const GROUPS = ['采购管理', '仓库管理', '财务管理'];
for (const [idx, group] of GROUPS.entries()) {
  try {
    await page.goto(`${BASE}/business/erp/inventory`, { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(2000);
    await dismissOverlays();
    const ok = await expandGroup(group);
    if (!ok) domLines.push(`!! 侧边栏里找不到目录「${group}」`);
    const rows = await sidebarRows();
    domLines.push(`=== 展开「${group}」===`);
    for (const r of rows) {
      const mark = r.kind === 'dir ' ? (r.open ? '▾' : '▸') : ' ';
      domLines.push(`${'    '.repeat(r.depth)}${mark} ${r.kind} ${r.text}`);
    }
    domLines.push('');
    const file = path.join(OUT, `02-sidebar-${idx + 1}-${group}.png`);
    await page.locator('.basic-layout__aside').screenshot({ path: file }).catch(async () => {
      await page.screenshot({ path: file });
    });
    console.log(`  [shot] ${path.relative('/opt/cloudpivot', file)}`);
  } catch (e) {
    domLines.push(`!! 展开「${group}」失败: ${e.message}`);
    console.log(`  !! 展开「${group}」失败: ${e.message}`);
  }
}
fs.writeFileSync(path.join(OUT, '02-sidebar-dom.txt'), domLines.join('\n') + '\n');
console.log(domLines.join('\n'));

// 逐页：打开页面 → 逐级展开所在分组 → 侧边栏截图 + 全屏截图 + 面包屑
for (const t of TARGETS) {
  const hit = flat.get(t.path);
  const chain = hit ? hit.chain.map((c) => c.title) : [];
  const r = { path: t.path, name: t.name, chain, domExpanded: null, crumb: null, shot: null };
  try {
    await page.goto(`${BASE}${t.path}`, { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(2200);
    await dismissOverlays();
    for (const g of chain) {
      const ok = await expandGroup(g);
      if (!ok) r.domExpanded = `展开失败：${g}`;
      await page.waitForTimeout(300);
    }
    const rows = await sidebarRows();
    // 侧边栏里该页面条目所处的一级分组（缩进 0 的最近 dir）
    let cur = null;
    let top = null;
    for (const row of rows) {
      if (row.kind === 'dir ' && row.depth === 0) { top = row.text; cur = top; continue; }
      if (row.kind === 'page' && row.text.includes(t.name)) { cur = top; break; }
    }
    r.sidebarGroup = cur;
    const file = path.join(OUT, `03-sidebar-${t.label}.png`);
    await page.locator('.basic-layout__aside').screenshot({ path: file }).catch(async () => {
      await page.screenshot({ path: file });
    });
    r.shot = path.relative('/opt/cloudpivot', file);
    const full = path.join(OUT, `04-page-${t.label}.png`);
    await page.screenshot({ path: full });
    r.fullShot = path.relative('/opt/cloudpivot', full);
    const crumb = await page.locator('.basic-layout__breadcrumb').innerText().catch(() => '');
    r.crumb = crumb.replace(/\s+/g, ' ').trim();
    fs.writeFileSync(path.join(OUT, `crumb-${t.label}.txt`), `${r.crumb}\n`);
    console.log(`  [page] ${t.name} 侧边栏分组=${cur} 面包屑=${r.crumb}`);
  } catch (e) {
    r.error = e.message;
    console.log(`  !! ${t.name} 采集失败: ${e.message}`);
  }
  pageResults.push(r);
}

fs.writeFileSync(path.join(OUT, 'page-results.json'), JSON.stringify(pageResults, null, 2));
await browser.close();

// ---------- ④ 结论 ----------
const bad = pageResults.filter((r) => r.error || (r.chain.length && r.sidebarGroup !== r.chain[0]));
console.log(`\n输出目录: ${OUT}`);
console.log(`API 断言失败: ${apiFail}`);
console.log(`侧边栏与 API 父链不一致/采集失败: ${bad.length}`);
if (apiFail || bad.length) process.exit(1);
