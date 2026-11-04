#!/usr/bin/env node
/**
 * VUE-FLOW-2B 定向实验：句柄命中区 & 落点容差（Vue Flow vs 老画布）。
 *
 * 复现用户真机动作「从右侧圆点拖到目标节点卡片上松手」：
 *   · 老画布：落点只要压在目标卡片上就成边（findDropTarget → hitTest 任意节点）
 *   · 新画布：Vue Flow 只认「落点在目标 Handle 的 connectionRadius（默认 20 画布 px）内」
 *
 * 该脚本量四件事：
 *   A. 句柄的真实命中半径（elementsFromPoint 扫描，屏幕 px）
 *   B. 落在句柄中心 → 是否成边（对照）
 *   C. 落在目标**卡片**中心 → 画布是否已经把目标高亮成「可落点」+ 松手后是否真的成边
 *   D. 落在句柄外 N 屏幕 px（仍在卡片内）→ 是否成边
 *
 * 用法：node scripts/verify/vueflow_drop_tolerance_diag.cjs --def-id <已发布流程 id>
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
      /* 下一个 */
    }
  }
  throw new Error('找不到 Playwright');
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
const BROWSER = (args.browser || 'chromium').toLowerCase();
const TAG = args.tag || 'after';
const EVID = 'docs/evidence/20261104-vueflow-2b';
if (!DEF_ID) {
  console.error('缺少 --def-id');
  process.exit(2);
}

const results = [];

async function main() {
  const playwright = loadPlaywright();
  const engine = playwright[BROWSER];
  const token = await loginToken(BASE);

  const browser = await engine.launch(BROWSER === 'chromium' ? { args: ['--no-sandbox'] } : {});
  const context = await browser.newContext({ viewport: { width: 1600, height: 1000 } });
  await context.addInitScript((value) => {
    localStorage.setItem('x_access_token', value);
    localStorage.setItem('cp-onboarding-done', '1');
    sessionStorage.setItem('login-unread-shown', '1');
  }, token);

  /** 打开编辑器（指定引擎），必要时先「另存为新草稿」拿到可编辑版本 */
  async function openEditor(canvasName) {
    const page = await context.newPage();
    await page.goto(`${BASE}/flow/def/${DEF_ID}/edit?canvas=${canvasName}`, { waitUntil: 'domcontentloaded' });
    await page.waitForSelector('.flow-node__card', { timeout: 30000 });
    await page.waitForTimeout(1200);
    if (canvasName === 'vueflow') {
      const draft = page.locator('button:has-text("另存为新草稿")');
      if (await draft.count()) {
        await draft.first().click();
        await page.waitForTimeout(2500);
        await page.waitForSelector('.vue-flow__node', { timeout: 30000 });
        await page.waitForTimeout(800);
      }
    }
    return page;
  }

  /** 当前画布的几何：句柄 / 卡片 / 缩放 */
  const geom = () => {
    const num = (v) => Math.round(v * 100) / 100;
    const rect = (el) => {
      const r = el.getBoundingClientRect();
      return { x: num(r.x), y: num(r.y), w: num(r.width), h: num(r.height), cx: num(r.x + r.width / 2), cy: num(r.y + r.height / 2) };
    };
    const wrap = document.querySelector('.flow-canvas');
    const pane = document.querySelector('.vue-flow__transformationpane');
    let zoom = 1;
    if (pane) {
      const m = getComputedStyle(pane).transform;
      zoom = m.startsWith('matrix(') ? Number(m.slice(7, -1).split(',')[0]) : 1;
    }
    return {
      zoom: Math.round(zoom * 1000) / 1000,
      vf: Boolean(document.querySelector('.vue-flow')),
      // vueflow：句柄；老画布：出口锚点 circle
      sources: [...document.querySelectorAll('.flow-node__port--out')].map((el) => ({
        node: el.closest('.vue-flow__node')?.getAttribute('data-id') ?? el.closest('[data-node-key]')?.getAttribute('data-node-key') ?? null,
        rect: rect(el),
      })),
      targets: [...document.querySelectorAll('.flow-node__port--in, .flow-node__port:not(.flow-node__port--out)')]
        .filter((el) => el.tagName === 'CIRCLE' || el.classList.contains('vue-flow__handle'))
        .map((el) => ({
          node: el.closest('.vue-flow__node')?.getAttribute('data-id') ?? el.closest('[data-node-key]')?.getAttribute('data-node-key') ?? null,
          rect: rect(el),
        })),
      cards: [...document.querySelectorAll('.flow-node__card, .flow-node__hit')].map((el) => ({
        node:
          el.getAttribute('data-node-key') ??
          el.closest('[data-node-key]')?.getAttribute('data-node-key') ??
          el.closest('.vue-flow__node')?.getAttribute('data-id') ??
          null,
        rect: rect(el),
        isHitLayer: el.classList.contains('flow-node__hit'),
      })),
      wrap: rect(wrap),
    };
  };

  const page = await openEditor('vueflow');
  const g = await page.evaluate(geom);
  console.log(`zoom=${g.zoom} 句柄=${g.sources.length} 卡片=${g.cards.length}`);
  results.push({ section: 'geometry', zoom: g.zoom, handles: g.sources.length });

  // ---------- A. 句柄命中半径（屏幕 px） ----------
  const probe = g.sources[0];
  const hits = await page.evaluate(
    ({ cx, cy }) => {
      const out = {};
      for (let d = 0; d <= 16; d += 1) {
        const el = document.elementFromPoint(cx + d, cy);
        out[d] = el ? `${el.tagName.toLowerCase()}.${String(el.className).split(' ').filter((c) => c.includes('handle') || c.includes('port') || c.includes('card') || c.includes('node') || c.includes('pane')).join('.')}` : null;
      }
      return out;
    },
    { cx: probe.rect.cx, cy: probe.rect.cy },
  );
  console.log('\nA. 从句柄中心向右扫（屏幕 px → 命中的元素）：');
  for (const [d, el] of Object.entries(hits)) console.log(`   +${String(d).padStart(2)}px → ${el}`);
  results.push({ section: 'hit-sweep', node: probe.node, sweep: hits });

  // ---------- 选一组「库里还没有」的 (from,to) ----------
  const pairs = await page.evaluate(() => {
    const el = document.querySelector('.vue-flow');
    let inst = el && el.__vueParentComponent;
    while (inst) {
      for (const bag of [inst.provides, inst.appContext && inst.appContext.provides]) {
        if (!bag) continue;
        for (const key of Object.getOwnPropertySymbols(bag).concat(Object.keys(bag))) {
          const v = bag[key];
          if (v && v.nodeLookup && v.nodeLookup.value instanceof Map) {
            return [...v.edges.value.values()].map((e) => `${e.source}→${e.target}`);
          }
        }
      }
      inst = inst.parent;
    }
    return [];
  });
  const existing = new Set(pairs);
  const used = new Set();

  /** 找一对：from 有 source 句柄、to 有 target 句柄、库里没有这条边、且本轮没用过 */
  function pickPair(g2, preferFar) {
    const sources = g2.sources;
    const targets = g2.targets;
    let best = null;
    let bestScore = preferFar ? -Infinity : Infinity;
    for (const s of sources) {
      for (const t of targets) {
        if (!s.node || !t.node || s.node === t.node) continue;
        if (existing.has(`${s.node}→${t.node}`)) continue;
        if (used.has(`${s.node}→${t.node}`)) continue;
        const d = Math.hypot(t.rect.cx - s.rect.cx, t.rect.cy - s.rect.cy);
        const score = preferFar ? d : d;
        const better = preferFar ? score > bestScore : score < bestScore;
        if (better) {
          bestScore = score;
          best = { s, t };
        }
      }
    }
    return best;
  }

  /** 当前视口变换（用来发现「坐标快照过期」） */
  async function viewOf(p = page) {
    return p.evaluate(() => {
      const pane = document.querySelector('.vue-flow__transformationpane');
      return pane ? getComputedStyle(pane).transform : null;
    });
  }

  /** 执行一次「按下 → 移到落点 → 松手」，返回边数与高亮状态 */
  async function drag(from, toNodeId, dropPoint, label) {
    const viewBefore = await viewOf();
    const edgesBefore = await page.evaluate(() => document.querySelectorAll('.vue-flow__edge').length);
    await page.mouse.move(from.rect.cx, from.rect.cy);
    await page.mouse.down();
    await page.waitForTimeout(60);
    await page.mouse.move(from.rect.cx + 4, from.rect.cy + 2, { steps: 2 });
    await page.waitForTimeout(60);
    const started = await page.evaluate(() => Boolean(document.querySelector('.vue-flow__connection')));
    await page.mouse.move(dropPoint.x, dropPoint.y, { steps: 18 });
    await page.waitForTimeout(120);
    const hover = await page.evaluate(() => ({
      highlighted: document.querySelectorAll('.is-connect-target').length,
      validHandle: Boolean(document.querySelector('.vue-flow__handle.valid')),
    }));
    // 拖拽进行中的截图（预览线还活着）：只对 B（落在句柄中心）这一条拍，避免一堆图
    if (label.startsWith('B')) {
      fs.mkdirSync(`${EVID}/shots`, { recursive: true });
      await page.screenshot({ path: `${EVID}/shots/drag-preview-${TAG}.png` });
    }
    await page.mouse.up();
    await page.waitForTimeout(500);
    const edgesAfter = await page.evaluate(() => document.querySelectorAll('.vue-flow__edge').length);
    const row = { section: 'drag', label, started, highlighted: hover.highlighted, validHandle: hover.validHandle, edgesBefore, edgesAfter, created: edgesAfter > edgesBefore, viewBefore, viewAfter: await viewOf() };
    results.push(row);
    console.log(
      `  ${label}: 起手=${started ? 'OK' : '✗'} 目标高亮=${hover.highlighted} 句柄valid=${hover.validHandle} 边 ${edgesBefore}→${edgesAfter} ${row.created ? '✓ 成边' : '✗ 未成边'}`,
    );
    if (!row.created) {
      console.log('    debug:', JSON.stringify({ viewBefore, viewAfter: row.viewAfter, dropPoint, from: from.rect, toNodeId }));
    }
    if (row.created) {
      used.add(`${from.node}→${toNodeId}`);
      existing.add(`${from.node}→${toNodeId}`);
    }
    return row;
  }

  // ---------- B/C/D ----------
  console.log('\nB/C/D. 落点容差：');
  {
    const pair = pickPair(g, false);
    if (pair) {
      const targetCard = g.cards.find((c) => c.node === pair.t.node && !c.isHitLayer);
      await drag(pair.s, pair.t.node, { x: pair.t.rect.cx, y: pair.t.rect.cy }, `B 落在目标句柄中心 (${pair.s.node}→${pair.t.node})`);
      void targetCard;
    }
  }
  {
    const pair = pickPair(g, true);
    if (pair) {
      const targetCard = g.cards.find((c) => c.node === pair.t.node && !c.isHitLayer) ?? g.cards.find((c) => c.node === pair.t.node);
      const before = await page.evaluate(() => document.querySelectorAll('.vue-flow__edge').length);
      await page.mouse.move(pair.s.rect.cx, pair.s.rect.cy);
      await page.mouse.down();
      await page.waitForTimeout(60);
      await page.mouse.move(pair.s.rect.cx + 4, pair.s.rect.cy + 2, { steps: 2 });
      await page.mouse.move(targetCard.rect.cx, targetCard.rect.cy, { steps: 18 });
      await page.waitForTimeout(150);
      const hover = await page.evaluate(() => ({
        highlighted: document.querySelectorAll('.is-connect-target').length,
        visual: (() => {
          const el = document.querySelector('.is-connect-target');
          return el ? getComputedStyle(el).borderStyle + ' / ' + getComputedStyle(el).borderColor : null;
        })(),
      }));
      await page.screenshot({ path: `${EVID}/shots/drop-on-card-hover-${TAG}.png` });
      await page.mouse.up();
      await page.waitForTimeout(500);
      const after = await page.evaluate(() => document.querySelectorAll('.vue-flow__edge').length);
      const toasts = await page.evaluate(() => [...document.querySelectorAll('.t-message')].map((el) => el.textContent.trim()));
      const row = { section: 'drag', label: `C 落在目标卡片中心 (${pair.s.node}→${pair.t.node})`, highlighted: hover.highlighted, hoverBorder: hover.visual, edgesBefore: before, edgesAfter: after, created: after > before, toasts };
      results.push(row);
      console.log(`  C 落在目标卡片中心 (${pair.s.node}→${pair.t.node}): 卡片高亮=${hover.highlighted} 边框=${hover.visual} 边 ${before}→${after} ${row.created ? '✓ 成边' : '✗ 未成边'} 提示=${JSON.stringify(toasts)}`);
    }
  }

  // ---------- D. 落点在句柄附近但离开句柄（仍在卡片上） ----------
  for (const offset of [6, 10, 14, 20]) {
    const pair = pickPair(g, false);
    if (!pair) break;
    await drag(pair.s, pair.t.node, { x: pair.t.rect.cx + offset, y: pair.t.rect.cy }, `D 落点在目标句柄右 ${offset}px (屏幕, ${pair.s.node}→${pair.t.node})`);
  }

  // ---------- E. 对照：老画布同样动作 ----------
  const nativePage = await openEditor('native');
  const ng = await nativePage.evaluate(geom);
  console.log(`\nE. 老画布对照（zoom=${ng.zoom}）：`);
  {
    // 老画布没有 store，用新画布拿到的「已有边」集合来避重复边
    const src = ng.sources.find((s) => s.node) ?? ng.sources[0];
    const card = ng.cards.find((c) => c.node && c.node !== src.node && c.isHitLayer && !existing.has(`${src.node}→${c.node}`));
    if (!card) {
      console.log('  找不到非重复边的目标卡片，跳过');
    } else {
    const before = await nativePage.evaluate(() => document.querySelectorAll('.flow-edge').length);
    await nativePage.mouse.move(src.rect.cx, src.rect.cy);
    await nativePage.mouse.down();
    await nativePage.waitForTimeout(60);
    await nativePage.mouse.move(src.rect.cx + 4, src.rect.cy + 2, { steps: 2 });
    await nativePage.waitForTimeout(60);
    const draft = await nativePage.evaluate(() => Boolean(document.querySelector('.flow-canvas__draft')));
    await nativePage.mouse.move(card.rect.cx, card.rect.cy, { steps: 18 });
    await nativePage.waitForTimeout(120);
    const highlight = await nativePage.evaluate(() => document.querySelectorAll('.is-connect-target').length);
    await nativePage.mouse.up();
    await nativePage.waitForTimeout(500);
    const after = await nativePage.evaluate(() => document.querySelectorAll('.flow-edge').length);
    const row = { section: 'drag-native', label: `E 老画布：落在目标卡片中心 (${src.node}→${card.node})`, started: draft, highlighted: highlight, edgesBefore: before, edgesAfter: after, created: after > before };
    results.push(row);
    console.log(`  起手预览=${draft} 卡片高亮=${highlight} 边 ${before}→${after} ${row.created ? '✓ 成边' : '✗ 未成边'}`);
    }
  }

  fs.mkdirSync('docs/evidence/20261104-vueflow-2b', { recursive: true });
  fs.writeFileSync(`${EVID}/drop-tolerance-${TAG}.json`, JSON.stringify(results, null, 2));
  console.log(`\n证据：${EVID}/drop-tolerance-${TAG}.json`);
  await browser.close();
}

main().catch((e) => {
  console.error('实验异常：', e);
  process.exit(3);
});
