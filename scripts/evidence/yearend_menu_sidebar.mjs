#!/usr/bin/env node
/**
 * P2-2B-FIX（t_d1a5324f）取证：侧边栏里「年度结转」挂在哪个分组下。
 *
 * 用法：
 *   MANUAL_BASE=http://127.0.0.1:5180 node scripts/evidence/yearend_menu_sidebar.mjs before|after
 *
 * 输出（OUT 目录，默认 docs/evidence/20261105-yearend-menu-reparent/<label>/）：
 *   01-menu-api-parent.txt   从 /api/system/auth/menus 解析出的「年度结转」父节点（侧边栏的数据源）
 *   menus-api.json           完整菜单树（原始响应，供人工复核）
 *   02-sidebar-dom.txt       侧边栏 DOM 层级文本（目录/页面 + 缩进 + 当前是否展开）
 *   03-sidebar-purchase.png  采购管理 展开时的侧边栏
 *   04-sidebar-finance.png   财务管理 展开时的侧边栏
 *   05-page-yearend.png      年度结转页面 + 侧边栏
 *
 * 只读：不点任何写库按钮，只切换侧边栏展开项。
 */
import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';
import fs from 'node:fs';
import path from 'node:path';

const BASE = process.env.MANUAL_BASE || 'http://127.0.0.1:5180';
const USER = process.env.MANUAL_USER || 'admin';
const PASS = process.env.MANUAL_PASSWORD || 'Manual@123456';
const label = process.argv[2] || 'run';
const OUT = process.env.OUT || path.join('/opt/cloudpivot/docs/evidence/20261105-yearend-menu-reparent', label);
fs.mkdirSync(OUT, { recursive: true });

const YEAR_END = '/business/erp/year-end';
const PURCHASE = '/business/erp/purchase-mgmt';
const FINANCE = '/business/erp/finance-mgmt';

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

function walk(nodes, parent, depth = 0, out = []) {
  for (const n of nodes || []) {
    out.push({ node: n, parent, depth });
    walk(n.children, n, depth + 1, out);
  }
  return out;
}
const flat = walk(menusBody.data, null);
const ye = flat.find((x) => x.node.path === YEAR_END);
const groups = flat.filter((x) => [PURCHASE, FINANCE].includes(x.node.path));
const line = (x) =>
  x ? `title=${x.node.title} path=${x.node.path} menu_type=${x.node.menu_type} sort=${x.sort_order ?? x.node.sort_order}` : '（未找到）';
const apiLines = [
  `实例: ${BASE}  账号: ${USER}  时间: ${new Date().toISOString()}`,
  `[API] 年度结转            → ${line(ye)}`,
  `[API] 年度结转 父节点      → ${ye?.parent ? `title=${ye.parent.title} path=${ye.parent.path} id=${ye.parent.id}` : '（顶层节点）'}`,
  ...groups.map(
    (m) =>
      `[API] 分组 ${m.node.title} → id=${m.node.id} 子节点=${(m.node.children || []).map((c) => c.title).join(' / ')}`,
  ),
];
fs.writeFileSync(path.join(OUT, '01-menu-api-parent.txt'), apiLines.join('\n') + '\n');
console.log(apiLines.join('\n'));

// ---------- ③ 浏览器：侧边栏截图 ----------
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

await page.goto(`${BASE}${YEAR_END}`, { waitUntil: 'domcontentloaded' });
await page.waitForTimeout(3000);
await dismissOverlays();
await page.waitForTimeout(400);
await dismissOverlays();

/**
 * 展开一级目录。TDesign 的 submenu 标题就是 `li.t-submenu > div.t-menu__item`
 * （不是 `.t-submenu__title`），点击它才是开关；t-menu 设了 expand-mutex，
 * 后点开的会收起前一个。
 */
async function expandGroup(title) {
  const li = page.locator('.basic-layout__aside li.t-submenu').filter({ hasText: title });
  const n = await li.count();
  let target = null;
  for (let i = 0; i < n; i++) {
    const cand = li.nth(i);
    const txt = (await cand.locator(':scope > div.t-menu__item').innerText().catch(() => '')).replace(/\s+/g, '');
    if (txt === title) {
      target = cand.locator(':scope > div.t-menu__item').first();
      break;
    }
  }
  if (!target) throw new Error(`侧边栏里找不到目录「${title}」`);
  await target.scrollIntoViewIfNeeded().catch(() => {});
  await target.click();
  await page.waitForTimeout(800);
  return page.locator('.basic-layout__aside li.t-submenu.t-is-opened > div.t-menu__item').first().innerText().then(
    (t) => t.replace(/\s+/g, ''),
    () => '',
  );
}

/** 侧边栏完整层级（含未展开子树的 DOM 结构） */
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
for (const [idx, group] of ['采购管理', '财务管理'].entries()) {
  let opened = '';
  try {
    opened = await expandGroup(group);
  } catch (e) {
    console.log(`  !! 展开「${group}」失败: ${e.message}`);
  }
  const rows = await sidebarRows();
  domLines.push(`=== 展开「${group}」（当前展开 = ${opened || '无'}）===`);
  for (const r of rows) {
    const mark = r.kind === 'dir ' ? (r.open ? '▾' : '▸') : ' ';
    domLines.push(`${'    '.repeat(r.depth)}${mark} ${r.kind} ${r.text}`);
  }
  domLines.push('');
  const file = path.join(OUT, idx === 0 ? '03-sidebar-purchase.png' : '04-sidebar-finance.png');
  await page.locator('.basic-layout__aside').screenshot({ path: file }).catch(async () => {
    await page.screenshot({ path: file });
  });
  console.log(`  [shot] ${path.relative('/opt/cloudpivot', file)}`);
}
fs.writeFileSync(path.join(OUT, '02-sidebar-dom.txt'), domLines.join('\n') + '\n');
console.log(domLines.join('\n'));

// 最后一屏：年度结转页 + 侧边栏（验证面包屑也归财务）
await page.goto(`${BASE}${YEAR_END}`, { waitUntil: 'domcontentloaded' });
await page.waitForTimeout(2500);
await dismissOverlays();
await page.waitForTimeout(300);
const crumb = await page.locator('.basic-layout__breadcrumb').innerText().catch(() => '');
console.log(`  [note] 面包屑=${crumb.replace(/\s+/g, ' ')}`);
const pageShot = path.join(OUT, '05-page-yearend.png');
await page.screenshot({ path: pageShot });
console.log(`  [shot] ${path.relative('/opt/cloudpivot', pageShot)}`);

const openedAfterLoad = await page
  .locator('.basic-layout__aside li.t-submenu.t-is-opened > div.t-menu__item')
  .first()
  .innerText()
  .catch(() => '');
// 面包屑是路由口径（「首页 > 年度结转」），与菜单分组无关，只打日志不入档：
// 判定走的是 01（菜单 API 父节点）与 03/04（侧边栏实测）。
console.log(`  [note] 打开年度结转页时自动展开的目录=${(openedAfterLoad || '无').replace(/\s+/g, '')}`);

await browser.close();
console.log(`\n输出目录: ${OUT}`);
