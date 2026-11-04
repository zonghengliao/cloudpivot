#!/usr/bin/env node
/**
 * VUE-FLOW-2B 诊断：编辑态「拖不出连线」的根因定位。
 *
 * 复现路径与用户真机一致：
 *   1. 打开**已发布**版本的编辑器（readonly=true，句柄不可连）
 *   2. 点「另存为新草稿」→ 新建 v(N+1) 草稿并切过去（readonly 应变 false）
 *   3. 从 source 句柄拖到 target 句柄 → 断言：预览线出现 / 边数 +1
 *
 * 同时把 Vue Flow store 里的 nodeLookup（含每个节点 handleBounds）dump 出来 ——
 * `handleBounds` 是 `useHandle.handlePointerDown()` 里 `getHandle()` 的唯一数据源：
 * 若为空，pointerdown 之后会**静默 return**（不报错、不出现预览线、不建边）。
 *
 * 用法：
 *   node scripts/verify/vueflow_draft_connect_diag.cjs --def-id <已发布流程 id> [--base-url http://127.0.0.1:5188]
 */
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { loginToken } = require('./vueflow_token.cjs');

function loadPlaywright() {
  const candidates = [
    '@playwright/test',
    path.join(__dirname, '..', '..', 'frontend', 'node_modules', '@playwright/test'),
    '/opt/cloudpivot/frontend/node_modules/@playwright/test',
    'playwright',
  ];
  for (const candidate of candidates) {
    try {
      return require(candidate);
    } catch {
      /* 换下一个 */
    }
  }
  throw new Error('找不到 Playwright；请在仓库内运行');
}

function parseArgs(argv) {
  const out = {};
  for (let i = 0; i < argv.length; i += 1) {
    const token = argv[i];
    if (!token.startsWith('--')) continue;
    const eq = token.indexOf('=');
    if (eq > 0) out[token.slice(2, eq)] = token.slice(eq + 1);
    else if (argv[i + 1] && !argv[i + 1].startsWith('--')) {
      out[token.slice(2)] = argv[i + 1];
      i += 1;
    } else out[token.slice(2)] = 'true';
  }
  return out;
}

const args = parseArgs(process.argv.slice(2));
const BASE = (args['base-url'] || 'http://127.0.0.1:5188').replace(/\/$/, '');
const DEF_ID = args['def-id'];
const OUT = args.out || '';
const USER = args.user || 'admin';
const PASS = args.password || 'Admin@123456';
const BROWSER = (args.browser || 'chromium').toLowerCase();

if (!DEF_ID) {
  console.error('缺少 --def-id');
  process.exit(2);
}

/** 页面内：从 Vue 组件树里把 Vue Flow store 挖出来（只读诊断，不依赖 devtools） */
const storeProbe = () => {
  const el = document.querySelector('.vue-flow');
  let inst = el && el.__vueParentComponent;
  const seen = new Set();
  while (inst) {
    const bags = [inst.provides, inst.appContext && inst.appContext.provides];
    for (const bag of bags) {
      if (!bag) continue;
      const keys = Object.getOwnPropertySymbols(bag).concat(Object.keys(bag));
      for (const key of keys) {
        let value;
        try {
          value = bag[key];
        } catch {
          continue;
        }
        if (!value || typeof value !== 'object') continue;
        if (seen.has(value)) continue;
        seen.add(value);
        const lookup = value.nodeLookup;
        if (lookup && lookup.value instanceof Map) return value;
      }
    }
    inst = inst.parent;
  }
  return null;
};

async function main() {
  const playwright = loadPlaywright();
  const engine = playwright[BROWSER];
  if (!engine) throw new Error(`Playwright 没有 ${BROWSER} 引擎`);

  const token = await loginToken(BASE, USER, PASS);

  const browser = await engine.launch(BROWSER === 'chromium' ? { args: ['--no-sandbox'] } : {});
  const context = await browser.newContext({ viewport: { width: 1600, height: 1000 } });
  await context.addInitScript((value) => {
    localStorage.setItem('x_access_token', value);
    localStorage.setItem('cp-onboarding-done', '1');
    sessionStorage.setItem('login-unread-shown', '1');
  }, token);

  const consoleErrors = [];
  const page = await context.newPage();
  page.on('console', (m) => {
    if (m.type() === 'error') consoleErrors.push(m.text());
  });
  page.on('pageerror', (e) => consoleErrors.push('PAGEERROR ' + e.message));

  const url = `${BASE}/flow/def/${DEF_ID}/edit?canvas=vueflow`;
  console.log('▶ 打开', url);
  await page.goto(url, { waitUntil: 'domcontentloaded' });
  await page.waitForSelector('.vue-flow__node', { timeout: 30000 });
  await page.waitForTimeout(1200);

  const snap = async (label) => {
    const data = await page.evaluate((fnSrc) => {
      const pickStore = new Function('return (' + fnSrc + ')()');
      const store = pickStore();
      const wrap = document.querySelector('.flow-canvas');
      const handles = [...document.querySelectorAll('.vue-flow__handle')].map((h) => {
        const r = h.getBoundingClientRect();
        return {
          node: h.getAttribute('data-nodeid'),
          handleId: h.getAttribute('data-handleid'),
          pos: h.getAttribute('data-handlepos'),
          type: h.classList.contains('source') ? 'source' : h.classList.contains('target') ? 'target' : '?',
          cls: h.className,
          rect: { x: Math.round(r.x * 10) / 10, y: Math.round(r.y * 10) / 10, w: Math.round(r.width * 10) / 10, h: Math.round(r.height * 10) / 10 },
        };
      });
      const nodes = [...document.querySelectorAll('.vue-flow__node')].map((n) => ({
        id: n.getAttribute('data-id'),
        rect: (() => {
          const r = n.getBoundingClientRect();
          return { x: Math.round(r.x), y: Math.round(r.y), w: Math.round(r.width), h: Math.round(r.height) };
        })(),
      }));
      const out = {
        wrapClass: wrap ? wrap.className : null,
        readonlyBanner: Boolean(document.querySelector('.flow-editor__alert')),
        alertText: document.querySelector('.flow-editor__alert')?.textContent?.replace(/\s+/g, ' ').trim().slice(0, 120) ?? null,
        hint: document.querySelector('.flow-canvas__hint')?.textContent?.replace(/\s+/g, ' ').trim() ?? null,
        handleCount: handles.length,
        nodes,
        handles,
        edgeCount: document.querySelectorAll('.vue-flow__edge').length,
        store: null,
      };
      if (store) {
        const lookup = store.nodeLookup.value;
        out.store = {
          nodesConnectable: store.nodesConnectable?.value ?? null,
          hasStore: true,
          pairs: [...store.edges.value.values()].map((e) => `${e.source}→${e.target}`),
          nodes: [...lookup.values()].map((n) => ({
            id: n.id,
            type: n.type,
            connectable: n.connectable,
            dimensions: n.dimensions,
            handleBounds: n.handleBounds
              ? {
                  source: (n.handleBounds.source || []).map((b) => ({ id: b.id, pos: b.position, x: Math.round(b.x * 10) / 10, y: Math.round(b.y * 10) / 10, w: Math.round(b.width), h: Math.round(b.height) })),
                  target: (n.handleBounds.target || []).map((b) => ({ id: b.id, pos: b.position, x: Math.round(b.x * 10) / 10, y: Math.round(b.y * 10) / 10, w: Math.round(b.width), h: Math.round(b.height) })),
                }
              : null,
            sourcePosition: n.sourcePosition,
            targetPosition: n.targetPosition,
          })),
        };
      }
      return out;
    }, storeProbe.toString());

    if (OUT) {
      fs.mkdirSync(path.dirname(OUT), { recursive: true });
      fs.writeFileSync(OUT, JSON.stringify({ label, at: new Date().toISOString(), data }, null, 2));
    }
    console.log(`\n===== ${label} =====`);
    console.log('wrap class           :', data.wrapClass);
    console.log('只读横幅             :', data.readonlyBanner, '|', data.alertText);
    console.log('提示文案             :', data.hint);
    console.log('节点 / 句柄 / 边      :', data.nodes.length, '/', data.handleCount, '/', data.edgeCount);
    if (data.store) {
      console.log('nodesConnectable     :', data.store.nodesConnectable);
      for (const n of data.store.nodes) {
        const hb = n.handleBounds;
        console.log(
          `  node ${String(n.id).slice(0, 12)} connectable=${n.connectable} dims=${n.dimensions?.width}x${n.dimensions?.height} ` +
            `handleBounds: source=${hb ? hb.source.length : 'null'} target=${hb ? hb.target.length : 'null'} ` +
            (hb && hb.source[0] ? `src0=${JSON.stringify(hb.source[0])}` : ''),
        );
      }
    } else {
      console.log('！！ 没从组件树里挖到 store');
    }
    const first = data.handles.slice(0, 2);
    for (const h of first) console.log('  handle:', JSON.stringify(h));
    return data;
  };

  const before = await snap('① 进入页面（只读态）');

  // ---- 点「另存为新草稿」 ----
  const draftButton = page.locator('button:has-text("另存为新草稿")');
  if (await draftButton.count()) {
    console.log('\n▶ 点「另存为新草稿」');
    await draftButton.first().click();
    await page.waitForTimeout(2500);
    await page.waitForSelector('.vue-flow__node', { timeout: 30000 });
    await page.waitForTimeout(1200);
  } else {
    console.log('\n（页面上没有「另存为新草稿」按钮，说明本来就不是只读态）');
  }

  const after = await snap('② 另存为草稿后');

  // ---- 尝试拖拽连线 ----
  const existing = new Set(after.store?.pairs ?? []);
  const nodesById = new Map();
  for (const h of after.handles) {
    if (!nodesById.has(h.node)) nodesById.set(h.node, {});
    nodesById.get(h.node)[h.type] = h;
  }
  /** 找一个「库里还没有」的 (from,to) 组合，离得越近越好，避免重复边被父组件按设计拒绝 */
  let source = null;
  let target = null;
  let bestDist = Infinity;
  for (const [fromId, ports] of nodesById) {
    if (!ports.source) continue;
    for (const [toId, targetPorts] of nodesById) {
      if (!targetPorts.target || toId === fromId) continue;
      if (existing.has(`${fromId}→${toId}`)) continue;
      const d = Math.hypot(targetPorts.target.rect.x - ports.source.rect.x, targetPorts.target.rect.y - ports.source.rect.y);
      if (d < bestDist) {
        bestDist = d;
        source = ports.source;
        target = targetPorts.target;
      }
    }
  }
  if (!source || !target) throw new Error('找不到可用的 source/target 句柄');
  const sx = source.rect.x + source.rect.w / 2;
  const sy = source.rect.y + source.rect.h / 2;
  const tx = target.rect.x + target.rect.w / 2;
  const ty = target.rect.y + target.rect.h / 2;
  console.log(`\n▶ 拖拽 ${source.node}(${source.type}) → ${target.node}(${target.type})  ${sx},${sy} → ${tx},${ty}`);

  const edgesBefore = await page.evaluate(() => document.querySelectorAll('.vue-flow__edge').length);
  await page.mouse.move(sx, sy);
  await page.mouse.down();
  await page.waitForTimeout(80);
  await page.mouse.move(sx + 5, sy + 2, { steps: 2 });
  await page.waitForTimeout(80);
  const midState = await page.evaluate(() => ({
    connection: Boolean(document.querySelector('.vue-flow__connection')),
    connectionPath: document.querySelector('.vue-flow__connection')?.getAttribute('d') ?? null,
    wrapClass: document.querySelector('.flow-canvas')?.className ?? null,
  }));
  console.log('  拖拽中：预览线存在 =', midState.connection, '| wrap =', midState.wrapClass);
  await page.mouse.move(tx, ty, { steps: 15 });
  await page.waitForTimeout(120);
  const hoverState = await page.evaluate(() => ({
    connection: Boolean(document.querySelector('.vue-flow__connection')),
    valid: Boolean(document.querySelector('.vue-flow__handle.valid')),
  }));
  console.log('  悬停目标：预览线 =', hoverState.connection, '| 目标句柄 valid =', hoverState.valid);
  await page.mouse.up();
  await page.waitForTimeout(600);
  const toasts = await page.evaluate(() =>
    [...document.querySelectorAll('.t-message, .t-toast__content, .t-notification')].map((el) => el.textContent.replace(/\s+/g, ' ').trim()),
  );
  if (toasts.length) console.log('  页面提示：', JSON.stringify(toasts));
  const edgesAfter = await page.evaluate(() => document.querySelectorAll('.vue-flow__edge').length);
  console.log(`  边数：${edgesBefore} → ${edgesAfter} ${edgesAfter > edgesBefore ? '（已创建）' : '（未创建 ✗）'}`);

  const shotDir = 'docs/evidence/20261104-vueflow-2b/shots';
  fs.mkdirSync(shotDir, { recursive: true });
  await page.screenshot({ path: path.join(shotDir, 'draft-after-drag.png'), fullPage: false });
  console.log('  截图：' + path.join(shotDir, 'draft-after-drag.png'));

  if (consoleErrors.length) {
    console.log('\n控制台错误：');
    for (const e of consoleErrors.slice(0, 10)) console.log('  ·', e.slice(0, 300));
  }

  const ok = edgesAfter > edgesBefore;
  console.log('\n结论：' + (ok ? '拖拽建边成功' : '拖拽建边失败（复现 ✓）'));
  await browser.close();
  process.exit(ok ? 0 : 1);
}

main().catch((e) => {
  console.error('诊断脚本异常：', e);
  process.exit(3);
});
