#!/usr/bin/env node
/**
 * Handle 诊断：**锚点方向**为什么必须是「右出左进」，以及它靠什么保证。
 *
 * 背景（真机 bug）：只读态曾用 `v-if="!readonly"` 把 `<Handle>` 摘掉，Vue Flow 拿不到句柄坐标
 * → `getHandlePosition()` 回落到节点的 `sourcePosition` / `targetPosition`，而这两个的默认值是
 * `Bottom` / `Top` → 所有连线变成「底部出、顶部进」。
 *
 * 本脚本验证三条：
 * 1. 编辑态：.vue-flow__handle = 2×节点数，data-handlepos 是 left / right，边从右边缘出、左边缘进；
 * 2. **兜底能力**：把 DOM 里的 handle 全删掉 + 触发重测 → 方向仍然对（节点上的 sourcePosition/
 *    targetPosition 生效）。这是「Handle 万一不渲染也不会退化」的直接证据；
 * 3. 只读态（比对台 readonly 场景）：handle 仍在 DOM（20 个）、opacity:0 不可见、方向仍正确。
 *
 * 用法：
 *   node scripts/verify/vueflow_handle_diag.cjs \
 *     --base-url http://127.0.0.1:5188 --def-id <草稿流程 id> \
 *     --shot-dir docs/evidence/20261104-vueflow-business/shots \
 *     --out docs/evidence/20261104-vueflow-business/handle-diag.json
 */
'use strict';

const fs = require('node:fs');
const path = require('node:path');

function loadPlaywright() {
  for (const candidate of ['@playwright/test', path.join(__dirname, '..', '..', 'frontend', 'node_modules', '@playwright/test'), '/opt/cloudpivot/frontend/node_modules/@playwright/test']) {
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
const DEF_ID = args['def-id'] || '';
const SHOT_DIR = args['shot-dir'] || 'docs/evidence/20261104-vueflow-business/shots';
const OUT = args.out || 'docs/evidence/20261104-vueflow-business/handle-diag.json';

const checks = [];
function check(name, ok, detail) {
  checks.push({ name, ok: Boolean(ok), detail: detail === undefined ? null : detail });
  console.log(`${ok ? 'PASS' : 'FAIL'} · ${name}${ok || detail === undefined ? '' : ' → ' + JSON.stringify(detail).slice(0, 500)}`);
}

/** 页面内：把每条边拆成「起点/终点」，并给出它们相对最近节点右/左边缘的偏差 */
const anchorProbe = (scope) => {
  const root = document.querySelector(scope) || document;
  const round = (v) => Math.round(v * 10) / 10;
  const paneEl = root.querySelector('.vue-flow__transformationpane') || root.querySelector('.flow-canvas__svg');
  const wrap = root.querySelector('.flow-canvas').getBoundingClientRect();
  const matrix = paneEl ? getComputedStyle(paneEl).transform : 'none';
  const nums = matrix.startsWith('matrix(') ? matrix.slice(7, -1).split(',').map(Number) : [1, 0, 0, 1, 0, 0];
  const zoom = nums[0] || 1;
  const nodes = [...root.querySelectorAll('.flow-node__card')].map((card) => {
    const rect = card.getBoundingClientRect();
    return {
      key: card.getAttribute('data-node-key') || card.closest('[data-node-key]')?.getAttribute('data-node-key') || card.closest('.vue-flow__node')?.getAttribute('data-id') || null,
      x: (rect.left - wrap.left - nums[4]) / zoom,
      y: (rect.top - wrap.top - nums[5]) / zoom,
      w: rect.width / zoom,
      h: rect.height / zoom,
    };
  });
  const handles = [...root.querySelectorAll('.vue-flow__handle')].map((handle) => ({
    pos: handle.getAttribute('data-handlepos'),
    opacity: getComputedStyle(handle).opacity,
    pointer: getComputedStyle(handle).pointerEvents,
  }));
  const lines = [...root.querySelectorAll('.flow-edge__line')].map((line) => {
    const d = line.getAttribute('d') ?? '';
    const nums2 = (d.match(/-?\d+(?:\.\d+)?/g) ?? []).map(Number);
    return { start: [nums2[0], nums2[1]], end: [nums2[nums2.length - 2], nums2[nums2.length - 1]] };
  });
  const nearest = (point, targetOf) => {
    let best = null;
    for (const node of nodes) {
      const target = targetOf(node);
      const dist = Math.hypot(point[0] - target[0], point[1] - target[1]);
      if (!best || dist < best.dist) best = { key: node.key, dist: round(dist) };
    }
    return best;
  };
  return {
    zoom,
    nodeCount: nodes.length,
    handleCount: handles.length,
    handlePositions: handles.map((item) => item.pos),
    handleOpacity: [...new Set(handles.map((item) => item.opacity))],
    handlePointer: [...new Set(handles.map((item) => item.pointer))],
    edges: lines.map((line) => ({
      start: line.start.map(round),
      end: line.end.map(round),
      fromRightEdge: nearest(line.start, (node) => [node.x + node.w, node.y + node.h / 2]),
      toLeftEdge: nearest(line.end, (node) => [node.x, node.y + node.h / 2]),
      fromBottomCenter: nearest(line.start, (node) => [node.x + node.w / 2, node.y + node.h]),
      toTopCenter: nearest(line.end, (node) => [node.x + node.w / 2, node.y]),
    })),
  };
};

/** 方向判定：起点落在「右边缘中点」附近，且明显比「底边中点」近（退化形态会反过来） */
function exitsRight(edge, tolerance = 8) {
  return edge.fromRightEdge.dist <= tolerance && edge.fromRightEdge.dist < edge.fromBottomCenter.dist / 2;
}

/** 方向判定：终点落在「左边缘中点」附近，且明显比「顶边中点」近 */
function entersLeft(edge, tolerance = 8) {
  return edge.toLeftEdge.dist <= tolerance && edge.toLeftEdge.dist < edge.toTopCenter.dist / 2;
}

(async () => {
  const playwright = loadPlaywright();
  const browser = await playwright[args.browser || 'chromium'].launch();
  const context = await browser.newContext({ viewport: { width: 1600, height: 1000 }, deviceScaleFactor: 1 });
  fs.mkdirSync(SHOT_DIR, { recursive: true });

  let token = null;
  try {
    token = fs.readFileSync('/tmp/vf2_token.txt', 'utf8').trim();
  } catch {
    /* 没有就用登录接口现取 */
  }
  if (!token) {
    const resp = await fetch(`${BASE}/api/system/auth/login`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username: 'admin', password: 'Admin@123456' }),
    });
    token = (await resp.json())?.data?.access_token;
  }
  await context.addInitScript((value) => {
    localStorage.setItem('x_access_token', value);
    localStorage.setItem('cp-onboarding-done', '1');
    sessionStorage.setItem('login-unread-shown', '1');
  }, token);

  const info = {};

  // ==================== ① 编辑态 ====================
  if (DEF_ID) {
    const page = await context.newPage();
    const errors = [];
    page.on('pageerror', (error) => errors.push(error.message));
    await page.goto(`${BASE}/flow/def/${DEF_ID}/edit?canvas=vueflow`, { waitUntil: 'domcontentloaded' });
    await page.waitForSelector('.flow-node__card', { timeout: 30000 });
    await page.waitForTimeout(1500);
    const before = await page.evaluate(anchorProbe, '[data-pane="vueflow"]');
    info.edit = before;
    check('编辑态：handle 数量 = 2 × 节点数', before.handleCount === before.nodeCount * 2, { handles: before.handleCount, nodes: before.nodeCount });
    check('编辑态：data-handlepos 只有 left / right（不是 top / bottom）', (() => {
      const set = new Set(before.handlePositions);
      return set.size === 2 && set.has('left') && set.has('right');
    })(), [...new Set(before.handlePositions)]);
    check('编辑态：每条边的起点都贴着节点右边缘（≤8px）、终点贴着左边缘（≤8px）', before.edges.every((edge) => exitsRight(edge) && entersLeft(edge)), before.edges.slice(0, 2));
    check('编辑态：没有任何一条边从底边中部出 / 顶边中部进', before.edges.every((edge) => edge.fromBottomCenter.dist > 20 && edge.toTopCenter.dist > 20), before.edges.slice(0, 2));
    await page.screenshot({ path: path.join(SHOT_DIR, 'diag-edit.png') });

    // ==================== ② 兜底：删掉 handle 后方向仍然正确 ====================
    const removed = await page.evaluate(() => {
      const handles = [...document.querySelectorAll('.vue-flow__handle')];
      const count = handles.length;
      for (const handle of handles) handle.remove();
      // 让 Vue Flow 重新测量节点：改一下卡片尺寸触发 ResizeObserver → updateNodeInternals
      for (const card of document.querySelectorAll('.flow-node__card')) {
        const height = card.getBoundingClientRect().height;
        card.style.height = `${Math.round(height) + 1}px`;
      }
      return count;
    });
    await page.waitForTimeout(1200);
    const after = await page.evaluate(anchorProbe, '[data-pane="vueflow"]');
    info.afterRemove = after;
    check('兜底：DOM 里的 handle 已清零（复现「只读态不渲染 Handle」）', removed > 0 && after.handleCount === 0, { removed, left: after.handleCount });
    check('兜底：handle 为 0 时连线仍从右边缘出、左边缘进（节点的 sourcePosition/targetPosition 生效）', after.edges.every((edge) => exitsRight(edge, 26) && entersLeft(edge, 26)), after.edges.filter((edge) => !(exitsRight(edge, 20) && entersLeft(edge, 20))).slice(0, 3));
    check('兜底：handle 为 0 时没有退化成「底边出 / 顶边进」', after.edges.every((edge) => edge.fromBottomCenter.dist > 20 && edge.toTopCenter.dist > 20), {
      worstBottom: after.edges.reduce((acc, edge) => Math.min(acc, edge.fromBottomCenter.dist), Infinity),
      worstTop: after.edges.reduce((acc, edge) => Math.min(acc, edge.toTopCenter.dist), Infinity),
    });
    await page.screenshot({ path: path.join(SHOT_DIR, 'diag-no-handle.png') });
    check('兜底验证期间 0 页面异常', errors.length === 0, errors);
    await page.close();
  } else {
    console.log('（跳过编辑态：未给 --def-id）');
  }

  // ==================== ③ 只读态（比对台 readonly 场景） ====================
  {
    const page = await context.newPage();
    await page.goto(`${BASE}/devtools/flow-vf2-compare/compare.html?case=readonly`, { waitUntil: 'domcontentloaded' });
    await page.waitForFunction(() => window.__ready === true, { timeout: 30000 });
    await page.evaluate(() => {
      for (const tag of document.querySelectorAll('.pane__tag')) tag.style.display = 'none';
      document.getElementById('events').style.display = 'none';
    });
    await page.waitForTimeout(300);
    const readonly = await page.evaluate(anchorProbe, '[data-pane="vueflow"]');
    info.readonly = readonly;
    check('只读态：handle 仍在 DOM（2 × 节点数），不是 0', readonly.handleCount === readonly.nodeCount * 2, { handles: readonly.handleCount, nodes: readonly.nodeCount });
    check('只读态：handle 的 data-handlepos 是 left / right', (() => {
      const set = new Set(readonly.handlePositions);
      return set.size === 2 && set.has('left') && set.has('right');
    })(), [...new Set(readonly.handlePositions)]);
    check('只读态：圆点不可见（opacity:0）且不吃指针', readonly.handleOpacity.join() === '0' && readonly.handlePointer.join() === 'none', {
      opacity: readonly.handleOpacity,
      pointer: readonly.handlePointer,
    });
    check('只读态：边 path 起点 ≈ 节点右边缘（不是底边中部）', readonly.edges.every((edge) => exitsRight(edge)), readonly.edges.filter((edge) => !exitsRight(edge)).slice(0, 3));
    check('只读态：边 path 终点 ≈ 节点左边缘（不是顶边中部）', readonly.edges.every((edge) => entersLeft(edge)), readonly.edges.filter((edge) => !entersLeft(edge)).slice(0, 3));
    const pane = await page.$('[data-pane="vueflow"]');
    await pane.screenshot({ path: path.join(SHOT_DIR, 'diag-readonly.png') });
    await page.close();
  }

  const report = { generatedAt: new Date().toISOString(), baseUrl: BASE, defId: DEF_ID || null, passed: checks.filter((c) => c.ok).length, failed: checks.filter((c) => !c.ok).length, checks, info };
  fs.mkdirSync(path.dirname(OUT), { recursive: true });
  fs.writeFileSync(OUT, JSON.stringify(report, null, 2));
  console.log(`\n${report.passed} passed / ${report.failed} failed · 报告 ${OUT}`);
  await browser.close();
  process.exit(report.failed === 0 ? 0 : 1);
})().catch((error) => {
  console.error(error);
  process.exit(2);
});
