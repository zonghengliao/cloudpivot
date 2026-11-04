#!/usr/bin/env node
/**
 * VUE-FLOW-2 真机验收：Vue Flow 画布的业务特性 vs 老画布（同一份数据、同一视口、同一页）。
 *
 * 两个阶段：
 *
 * ## A. 比对台（`frontend/devtools/flow-vf2-compare/compare.html`）
 *
 * 同一页并排渲染两套画布（视口都是 40,40 / 100%，不 fitView），于是两张画布的
 * 「元素截图」可以**直接逐像素相减**（不涉及 fitView 取整 / 重采样带来的噪声）。
 * 覆盖卡面 ①–⑥、⑧、⑩、⑪ 与「视觉并排 + 像素差」全部条目。
 *
 * ## B. 真实编辑器页（`/flow/def/:id/edit`）
 *
 * 覆盖从左侧面板拖拽创建（⑦，调用方 `def/edit.vue` 用 `canvasRef.screenToCanvas` 落点）、
 * 键盘（⑨：Delete / 空格平移 / Esc）以及跨引擎的行为等价。
 *
 * # 用法
 *
 *   node scripts/verify/flow_vf2_business_verify.cjs \
 *     --base-url http://127.0.0.1:5188 --def-id <草稿流程 id> \
 *     --shot-dir docs/evidence/20261104-vueflow-business/shots \
 *     --out docs/evidence/20261104-vueflow-business/report.json
 *
 * 不带 `--def-id` 时只跑 A 阶段。退出码 0 = 全通过，1 = 有用例失败。
 */
'use strict';

const fs = require('node:fs');
const path = require('node:path');

/** 找 Playwright（本仓 frontend/node_modules 里有一份） */
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

/** 解析 `--key value` / `--key=value` */
function parseArgs(argv) {
  const out = {};
  for (let i = 0; i < argv.length; i += 1) {
    const token = argv[i];
    if (!token.startsWith('--')) continue;
    const eq = token.indexOf('=');
    if (eq > 0) {
      out[token.slice(2, eq)] = token.slice(eq + 1);
    } else if (argv[i + 1] && !argv[i + 1].startsWith('--')) {
      out[token.slice(2)] = argv[i + 1];
      i += 1;
    } else {
      out[token.slice(2)] = 'true';
    }
  }
  return out;
}

const args = parseArgs(process.argv.slice(2));
const BASE = (args['base-url'] || 'http://127.0.0.1:5188').replace(/\/$/, '');
const DEF_ID = args['def-id'] || '';
const SHOT_DIR = args['shot-dir'] || 'docs/evidence/20261104-vueflow-business/shots';
const OUT = args.out || 'docs/evidence/20261104-vueflow-business/report.json';
const USER = args.user || 'admin';
const PASS = args.password || 'Admin@123456';
const BROWSER = (args.browser || 'chromium').toLowerCase();
const HARNESS = '/devtools/flow-vf2-compare/compare.html';

const checks = [];
function check(name, ok, detail) {
  checks.push({ name, ok: Boolean(ok), detail: detail === undefined ? null : detail });
  const brief = detail === undefined || ok ? '' : ' → ' + JSON.stringify(detail).slice(0, 600);
  console.log(`${ok ? 'PASS' : 'FAIL'} · ${name}${brief}`);
}

/** 页面内探针：两套画布的同一套口径 */
const probe = () => {
  const round = (v) => Math.round(v * 10) / 10;
  const pane = document.querySelector('[data-pane="vueflow"]');
  void pane;
  const out = {};
  for (const engine of ['native', 'vueflow']) {
    const host = document.querySelector(`[data-pane="${engine}"]`);
    const wrap = host.querySelector('.flow-canvas');
    const wr = wrap.getBoundingClientRect();
    const rel = (r) => [round(r.left - wr.left), round(r.top - wr.top), round(r.width), round(r.height)];
    // 老画布把 data-node-key 挂在 <g class="flow-node"> 上，新画布挂在卡片自己身上；
    // Handle / 角标是卡片的兄弟节点 → 再往上找 Vue Flow 的 .vue-flow__node[data-id]
    const keyOf = (el) => {
      if (!el) return null;
      const own = el.getAttribute('data-node-key');
      if (own) return own;
      const byAttr = el.closest('[data-node-key]');
      if (byAttr) return byAttr.getAttribute('data-node-key');
      const vf = el.closest('.vue-flow__node');
      if (vf) return vf.getAttribute('data-id');
      const group = el.closest('.flow-node');
      return group ? group.getAttribute('data-node-key') : null;
    };
    /** 有效透明度：老画布把 opacity 写在 <g class="flow-node"> 上，新画布写在卡片上 */
    const effectiveOpacity = (el) => {
      let value = 1;
      for (let node = el; node && node !== host; node = node.parentElement) {
        value *= Number(getComputedStyle(node).opacity || 1);
      }
      return String(Math.round(value * 1000) / 1000);
    };
    const cards = [...host.querySelectorAll('.flow-node__card')].map((card) => {
      const cs = getComputedStyle(card);
      const status = card.querySelector('.flow-node__status');
      const statusAfter = status ? getComputedStyle(status, '::after') : null;
      return {
        key: keyOf(card),
        type: card.getAttribute('data-node-type') || card.closest('[data-node-type]')?.getAttribute('data-node-type') || null,
        rect: rel(card.getBoundingClientRect()),
        radius: cs.borderRadius,
        padding: cs.padding,
        border: `${cs.borderStyle} ${cs.borderColor}`,
        opacity: effectiveOpacity(card),
        title: card.querySelector('.flow-node__title')?.textContent ?? null,
        sub: card.querySelector('.flow-node__sub')?.textContent ?? null,
        icon: card.querySelector('.flow-node__icon')?.textContent ?? null,
        iconBg: card.querySelector('.flow-node__icon') ? getComputedStyle(card.querySelector('.flow-node__icon')).backgroundColor : null,
        barWidth: card.querySelector('.flow-node__bar') ? round(card.querySelector('.flow-node__bar').getBoundingClientRect().width) : null,
        statusText: status?.textContent ?? null,
        statusTone: status?.className.match(/flow-node__status--([a-z-]+)/)?.[1] ?? null,
        statusBg: status ? getComputedStyle(status).backgroundColor : null,
        pulse: statusAfter ? statusAfter.animationName : null,
        selected: card.className.includes('is-selected'),
        shadow: cs.boxShadow !== 'none',
      };
    });
    const ports = [...host.querySelectorAll('.flow-node__port')].map((port) => {
      // 老画布是 <circle stroke>；新画布是 Handle 里内嵌的 <circle>（描边在子元素上）
      const dot = port.querySelector('.flow-node__port-dot');
      return {
        node: keyOf(port),
        side: port.getAttribute('class').includes('--out') || port.getAttribute('class').includes('handle-right') ? 'out' : 'in',
        rect: rel(port.getBoundingClientRect()),
        dotRect: rel((dot ?? port).getBoundingClientRect()),
        pointer: getComputedStyle(port).pointerEvents,
        opacity: getComputedStyle(port).opacity,
        handlePos: port.getAttribute('data-handlepos') ?? null,
        border: port.tagName === 'circle' ? getComputedStyle(port).stroke : dot ? getComputedStyle(dot).stroke : getComputedStyle(port).borderColor,
        strokeWidth: port.tagName === 'circle' ? getComputedStyle(port).strokeWidth : dot ? getComputedStyle(dot).strokeWidth : getComputedStyle(port).borderWidth,
        width: port.tagName === 'circle' ? round(port.getBoundingClientRect().width) : getComputedStyle(port).width,
      };
    });
    const badges = [...host.querySelectorAll('.flow-node__issue')].map((badge) => ({
      node: keyOf(badge),
      rect: rel(badge.getBoundingClientRect()),
      text: badge.textContent,
      title: badge.getAttribute('title'),
      bg: badge.tagName === 'g' ? getComputedStyle(badge.querySelector('circle')).fill : getComputedStyle(badge).backgroundColor,
    }));
    out[engine] = {
      engine: host.querySelector('.vue-flow') ? 'vueflow' : 'native',
      wrap: [round(wr.width), round(wr.height)],
      // 视口变换（画布逻辑坐标 = (屏幕相对坐标 - 平移) / 缩放）
      view: (() => {
        const paneEl = host.querySelector('.vue-flow__transformationpane') || host.querySelector('.flow-canvas__svg');
        const matrix = paneEl ? getComputedStyle(paneEl).transform : 'none';
        const nums = matrix.startsWith('matrix(') ? matrix.slice(7, -1).split(',').map(Number) : [1, 0, 0, 1, 0, 0];
        return { zoom: nums[0], x: nums[4], y: nums[5] };
      })(),
      cards,
      ports,
      badges,
      lines: [...host.querySelectorAll('.flow-edge__line')].map((line) => ({
        stroke: getComputedStyle(line).stroke,
        width: round(parseFloat(getComputedStyle(line).strokeWidth)),
        dash: getComputedStyle(line).strokeDasharray,
        marker: line.getAttribute('marker-end') || '',
        d: line.getAttribute('d'),
      })),
      labels: [...host.querySelectorAll('.flow-edge__label text')].map((text) => ({
        text: text.textContent,
        rect: rel(text.getBoundingClientRect()),
        size: getComputedStyle(text).fontSize,
      })),
      labelBox: [...host.querySelectorAll('.flow-edge__label rect')].map((rect) => {
        const cs = getComputedStyle(rect);
        return { fill: cs.fill, stroke: cs.stroke, radius: rect.getAttribute('rx') };
      }),
      hint: host.querySelector('.flow-canvas__hint')?.textContent?.replace(/\s+/g, ' ').trim() ?? null,
      minimap: Boolean(host.querySelector('.flow-canvas__minimap')),
      zoom: host.querySelector('.flow-canvas__zoom-btn--scale')?.textContent?.trim() ?? null,
      layoutActions: host.querySelectorAll('.flow-canvas__action').length,
    };
  }
  return out;
};

/** 用页面内 canvas 逐像素比两张图（data URL） */
async function pixelDiff(page, shotA, shotB, regions) {
  return page.evaluate(
    async ([a, b, rects]) => {
      const load = (src) =>
        new Promise((resolve, reject) => {
          const img = new Image();
          img.onload = () => resolve(img);
          img.onerror = reject;
          img.src = src;
        });
      const pixels = async (src) => {
        const img = await load(src);
        const canvas = document.createElement('canvas');
        canvas.width = img.width;
        canvas.height = img.height;
        const ctx = canvas.getContext('2d');
        ctx.drawImage(img, 0, 0);
        return { data: ctx.getImageData(0, 0, img.width, img.height).data, width: img.width, height: img.height };
      };
      const A = await pixels(a);
      const B = await pixels(b);
      const diffRegion = (rect, dx = 0, dy = 0) => {
        const x1 = Math.max(0, Math.round(rect[0]));
        const y1 = Math.max(0, Math.round(rect[1]));
        const x2 = Math.min(A.width, Math.round(rect[0] + rect[2]));
        const y2 = Math.min(A.height, Math.round(rect[1] + rect[3]));
        let sum = 0;
        let big = 0;
        let count = 0;
        for (let y = y1; y < y2; y += 1) {
          for (let x = x1; x < x2; x += 1) {
            const sx = x + dx;
            const sy = y + dy;
            if (sx < 0 || sy < 0 || sx >= B.width || sy >= B.height) continue;
            const i = (y * A.width + x) * 4;
            const j = (sy * B.width + sx) * 4;
            sum += (Math.abs(A.data[i] - B.data[j]) + Math.abs(A.data[i + 1] - B.data[j + 1]) + Math.abs(A.data[i + 2] - B.data[j + 2])) / 3;
            const delta = (Math.abs(A.data[i] - B.data[j]) + Math.abs(A.data[i + 1] - B.data[j + 1]) + Math.abs(A.data[i + 2] - B.data[j + 2])) / 3;
            if (delta > 40) big += 1;
            count += 1;
          }
        }
        return { mean: count ? sum / count : 0, bigRatio: count ? big / count : 0, count };
      };
      const out = [];
      for (const region of rects) {
        const raw = diffRegion(region.rect);
        // 同一视口下两边应当完全对齐；仍按 ±2px 搜一次最优，仅用于报告「是否真的对齐」
        let best = { dx: 0, dy: 0, ...raw };
        for (let dy = -2; dy <= 2; dy += 1) {
          for (let dx = -2; dx <= 2; dx += 1) {
            if (dx === 0 && dy === 0) continue;
            const hit = diffRegion(region.rect, dx, dy);
            if (hit.mean < best.mean) best = { dx, dy, ...hit };
          }
        }
        out.push({ key: region.key, raw, best });
      }
      return { size: [A.width, A.height], regions: out };
    },
    ['data:image/png;base64,' + shotA.toString('base64'), 'data:image/png;base64,' + shotB.toString('base64'), regions],
  );
}

/** 从 DOM 事实里挑出可比较的字段（忽略 key 顺序差） */
function byKey(list) {
  const map = new Map();
  for (const item of list) {
    if (item.key) map.set(item.key, item);
  }
  return map;
}

/** 数值数组的最大差 */
function maxDelta(a, b) {
  if (!a || !b || a.length !== b.length) return Infinity;
  return a.reduce((acc, value, index) => Math.max(acc, Math.abs(value - b[index])), 0);
}

/** 从 SVG path 的 `d` 里抽出所有数字（用于「逐点比较几何」而不是逐字符比较） */
function pathNumbers(d) {
  return (String(d ?? '').match(/-?\d+(?:\.\d+)?/g) ?? []).map(Number);
}

/**
 * 连线两端的**锚点方向**（右出 / 左进）。这是「锚点退化」的守门员。
 *
 * 为什么不是逐点比较 `d`：
 * Vue Flow 把 sourceX 算在 Handle 盒的**外边缘**（nodeRight + 6.25），老画布算在节点边缘，
 * 两者差 6.25px —— 但这段正好被半径 5.75 的锚点圆点盖住，肉眼与像素都看不出来
 * （实测「锚点区域平均像素差」与整图像素差都达标）。真正会毁掉观感的是**方向退化**
 * （Handle 缺失 → 回落到节点默认的 bottom/top → 所有线变成「底部出、顶部进」）。
 * 所以这里断言的是方向：起点必须落在某个节点的「右边缘中点」、终点落在「左边缘中点」，
 * 且不允许出现「底边中点出 / 顶边中点进」。
 */
function edgeAnchors(engine) {
  const view = engine.view ?? { x: 0, y: 0, zoom: 1 };
  const cards = engine.cards
    .filter((card) => card.key)
    .map((card) => ({
      key: card.key,
      x: (card.rect[0] - view.x) / view.zoom,
      y: (card.rect[1] - view.y) / view.zoom,
      w: card.rect[2] / view.zoom,
      h: card.rect[3] / view.zoom,
    }));
  /** 容差 8px：容纳「Handle 盒外边缘」那 6.25px，同时远小于卡片半宽（≥48px），不会把方向认错 */
  const near = (point, target) => Math.hypot(point[0] - target[0], point[1] - target[1]) <= 8;
  return engine.lines.map((line, index) => {
    const nums = pathNumbers(line.d);
    const start = [nums[0], nums[1]];
    const end = [nums[nums.length - 2], nums[nums.length - 1]];
    const from = cards.find((card) => near(start, [card.x + card.w, card.y + card.h / 2]));
    const to = cards.find((card) => near(end, [card.x, card.y + card.h / 2]));
    // 退化形态：起点在底边中点 / 终点在顶边中点（= Vue Flow 拿不到 Handle 时的默认值）
    const bottomOut = cards.find((card) => near(start, [card.x + card.w / 2, card.y + card.h]));
    const topIn = cards.find((card) => near(end, [card.x + card.w / 2, card.y]));
    return {
      index,
      start,
      end,
      from: from?.key ?? null,
      to: to?.key ?? null,
      bottomOut: bottomOut?.key ?? null,
      topIn: topIn?.key ?? null,
    };
  });
}

/** 比较两套画布每条边的「从谁到谁」是否完全一致（方向 + 端点都对） */
function compareEdgeAnchors(native, vue) {
  const a = edgeAnchors(native);
  const b = edgeAnchors(vue);
  const degenerate = b.filter((item) => !item.from || !item.to || item.bottomOut || item.topIn);
  const mismatched = b
    .map((item, index) => ({ index, vue: `${item.from}→${item.to}`, native: `${a[index]?.from}→${a[index]?.to}` }))
    .filter((item) => item.vue !== item.native);
  return {
    ok: degenerate.length === 0 && mismatched.length === 0 && b.length === a.length,
    count: b.length,
    degenerate: degenerate.slice(0, 4),
    mismatched: mismatched.slice(0, 4),
    pairs: b.map((item) => `${item.from}→${item.to}`),
  };
}

(async () => {
  const playwright = loadPlaywright();
  const engine = playwright[BROWSER];
  if (!engine) throw new Error('不支持的浏览器：' + BROWSER);
  fs.mkdirSync(SHOT_DIR, { recursive: true });

  const browser = await engine.launch();
  const context = await browser.newContext({ viewport: { width: 1680, height: 1400 }, deviceScaleFactor: 1 });
  const diffPage = await context.newPage();
  await diffPage.goto('about:blank');

  const consoleErrors = { harness: [], editor: [] };
  const shots = {};

  /** 打开比对台某个场景，隐藏场景标签（否则两张图的左上角文字不同） */
  async function openCase(name) {
    const page = await context.newPage();
    page.on('console', (message) => {
      if (message.type() === 'error') consoleErrors.harness.push(`${name}: ${message.text()}`);
    });
    page.on('pageerror', (error) => consoleErrors.harness.push(`${name}: PAGEERROR ${error.message}`));
    await page.goto(`${BASE}${HARNESS}?case=${name}`, { waitUntil: 'domcontentloaded' });
    await page.waitForFunction(() => window.__ready === true, { timeout: 30000 });
    await page.evaluate(() => {
      for (const tag of document.querySelectorAll('.pane__tag')) tag.style.display = 'none';
      document.getElementById('events').style.display = 'none';
    });
    await page.waitForTimeout(250);
    return page;
  }

  /** 截两张画布 + 逐像素比 */
  async function captureAndDiff(page, name, regions) {
    const nativeEl = await page.$('[data-pane="native"] .flow-canvas');
    const vueEl = await page.$('[data-pane="vueflow"] .flow-canvas');
    const nativePath = path.join(SHOT_DIR, `${name}-native.png`);
    const vuePath = path.join(SHOT_DIR, `${name}-vueflow.png`);
    const nativeBuf = await nativeEl.screenshot({ path: nativePath });
    const vueBuf = await vueEl.screenshot({ path: vuePath });
    const diff = await pixelDiff(diffPage, nativeBuf, vueBuf, regions);
    shots[name] = { native: nativePath, vueflow: vuePath };
    return diff;
  }

  /** 统计每个引擎的 emit 日志 */
  async function events(page, canvas) {
    return page.evaluate((which) => (window.__events || []).filter((item) => item.canvas === which), canvas);
  }
  async function clearEvents(page) {
    await page.evaluate(() => {
      window.__events.length = 0;
    });
  }

  const info = {};

  // ==================== 场景 1：plain（基线视差 + 锚点 / 标签 / 提示 / 空态） ====================
  {
    const page = await openCase('plain');
    const probed = await page.evaluate(probe);
    info.plain = probed;
    const native = probed.native;
    const vue = probed.vueflow;

    check('比对台：两侧都渲染出画布（native / vueflow）', native.engine === 'native' && vue.engine === 'vueflow', { native: native.engine, vue: vue.engine });
    check('节点数量一致（10 个）', native.cards.length === 10 && vue.cards.length === 10, { native: native.cards.length, vue: vue.cards.length });
    check('边数量一致（10 条）', native.lines.length === 10 && vue.lines.length === 10, { native: native.lines.length, vue: vue.lines.length });

    // 逐卡片：位置 / 尺寸 / 圆角 / 色条 / 遮罩
    const nMap = byKey(native.cards);
    const vMap = byKey(vue.cards);
    let worstRect = { key: null, delta: 0 };
    let worstBar = { key: null, delta: 0 };
    let sameStyle = true;
    const styleDiff = [];
    for (const [key, card] of nMap) {
      const peer = vMap.get(key);
      if (!peer) {
        sameStyle = false;
        styleDiff.push(`${key}:missing`);
        continue;
      }
      const rectDelta = maxDelta(card.rect, peer.rect);
      if (rectDelta > worstRect.delta) worstRect = { key, delta: rectDelta };
      const barDelta = Math.abs((card.barWidth ?? 0) - (peer.barWidth ?? 0));
      if (barDelta > worstBar.delta) worstBar = { key, delta: barDelta };
      for (const field of ['radius', 'padding', 'border', 'opacity', 'icon', 'iconBg', 'title', 'sub']) {
        if (String(card[field]) !== String(peer[field])) {
          sameStyle = false;
          styleDiff.push(`${key}.${field}: ${card[field]} ≠ ${peer[field]}`);
        }
      }
    }
    check('10 张卡片的位置 / 尺寸完全一致（≤0.6px）', worstRect.delta <= 0.6, worstRect);
    check('左侧色条宽度一致（3.5px）', worstBar.delta <= 0.6 && Math.abs((nMap.get('appr1')?.barWidth ?? 0) - 3.5) <= 0.6, worstBar);
    check('卡片样式（圆角/内距/边框/图标底色/主副标题）逐字一致', sameStyle, styleDiff.slice(0, 8));

    // 锚点：数量 / 位置 / 外观
    const inPorts = (list) => list.filter((item) => item.side === 'in');
    const outPorts = (list) => list.filter((item) => item.side === 'out');
    check('入口 / 出口锚点各 10 个（非只读）', inPorts(native.ports).length === 10 && inPorts(vue.ports).length === 10 && outPorts(vue.ports).length === 10, {
      native: native.ports.length,
      vue: vue.ports.length,
    });
    let worstPortDelta = { key: null, delta: 0 };
    for (const [key, card] of nMap) {
      const cardPorts = vue.ports.filter((port) => port.node === key);
      const peers = native.ports.filter((port) => port.node === key);
      if (cardPorts.length !== 2 || peers.length !== 2) {
        worstPortDelta = { key, delta: Infinity };
        continue;
      }
      // 老画布锚点圆心 = (x, y+h/2) / (x+w, y+h/2)，r=5
      const expected = [
        [card.rect[0], card.rect[1] + card.rect[3] / 2],
        [card.rect[0] + card.rect[2], card.rect[1] + card.rect[3] / 2],
      ];
      cardPorts.forEach((port, index) => {
        const center = [port.rect[0] + port.rect[2] / 2, port.rect[1] + port.rect[3] / 2];
        const delta = maxDelta(center, expected[index]);
        if (delta > worstPortDelta.delta) worstPortDelta = { key, delta };
      });
    }
    check('锚点落在卡片左/右边缘垂直中点（≤0.8px）', worstPortDelta.delta <= 0.8, worstPortDelta);
    check('锚点是可拖拽的（pointer-events: all）且为品牌色描边', vue.ports.every((port) => port.pointer === 'all') && vue.ports.every((port) => port.border === native.ports[0].border), {
      pointer: [...new Set(vue.ports.map((port) => port.pointer))],
      vueBorder: vue.ports[0]?.border,
      nativeBorder: native.ports[0]?.border,
    });
    check('锚点尺寸与老画布 circle(r=5, stroke 1.5) 一致（外径 11.5px，±0.6）', vue.ports.every((port) => Math.abs(parseFloat(port.width) - 11.5) <= 0.6), [...new Set(vue.ports.map((port) => port.width))]);
    check('锚点描边 1.5px、白底、品牌色（与老画布同值）', vue.ports.every((port) => port.strokeWidth === native.ports[0].strokeWidth && port.border === native.ports[0].border), {
      vue: [vue.ports[0]?.strokeWidth, vue.ports[0]?.border],
      native: [native.ports[0]?.strokeWidth, native.ports[0]?.border],
    });

    // hover 放大
    const firstOut = await page.$('[data-pane="vueflow"] .flow-node__port--out');
    const box = await firstOut.boundingBox();
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.waitForTimeout(220);
    const scaleOnHover = await page.evaluate(() => {
      const port = document.querySelector('[data-pane="vueflow"] .flow-node__port--out:hover');
      return port ? getComputedStyle(port).scale : null;
    });
    check('锚点 hover 放大（scale 1.25）', scaleOnHover === '1.25', scaleOnHover);
    await page.mouse.move(2, 2);
    await page.waitForTimeout(150);

    // 条件标签
    check(
      '条件标签文案一致（含兜底「其他情况」）',
      JSON.stringify(native.labels.map((item) => item.text).sort()) === JSON.stringify(vue.labels.map((item) => item.text).sort()) && vue.labels.length === 3,
      { native: native.labels.map((item) => item.text), vue: vue.labels.map((item) => item.text) },
    );
    let worstLabel = 0;
    for (const label of vue.labels) {
      const peer = native.labels.find((item) => item.text === label.text);
      if (!peer) continue;
      worstLabel = Math.max(worstLabel, maxDelta(label.rect, peer.rect));
    }
    check('标签位于边的中点、两套画布位置一致（≤0.6px）', worstLabel <= 0.6, worstLabel);
    check(
      '标签样式：白底 + 1px 边框 + 3px 圆角 + 10.5px 字',
      vue.labelBox.length === 3 && vue.labelBox.every((box2) => box2.radius === '3') && vue.labels.every((item) => item.size === native.labels[0].size),
      { radius: vue.labelBox.map((item) => item.radius), size: vue.labels[0]?.size, nativeSize: native.labels[0]?.size },
    );
    // 虚线 / 实线
    const nativeDash = native.lines.filter((line) => line.dash && line.dash !== 'none').length;
    const vueDash = vue.lines.filter((line) => line.dash && line.dash !== 'none').length;
    check('兜底分支虚线、实际分支实线（虚线条数一致）', nativeDash === vueDash && vueDash === 1, { nativeDash, vueDash });
    check('箭头 marker 与线宽一致', vue.lines.every((line) => Math.abs(line.width - 1.9) < 0.01) && native.lines.every((line) => Math.abs(line.width - 1.9) < 0.01), {
      vue: [...new Set(vue.lines.map((line) => line.width))],
      native: [...new Set(native.lines.map((line) => line.width))],
    });
    check(
      '底部操作提示文案逐字一致',
      vue.hint === native.hint && vue.hint === '拖节点移动 · 从右侧圆点拖出连线 · 滚轮缩放 · 空格/空白处拖拽平移 · Delete 删除',
      { vue: vue.hint, native: native.hint },
    );
    check('小地图与缩放工具栏都在（100%）', vue.minimap && vue.zoom === '100%' && native.zoom === '100%', { minimap: vue.minimap, vue: vue.zoom, native: native.zoom });

    // 像素差：整画布 + 每张卡片 + 每个锚点 + 每条标签
    const regions = [
      { key: '__whole', rect: [0, 0, vue.wrap[0], vue.wrap[1]] },
      ...vue.cards.filter((card) => card.key).map((card) => ({ key: card.key, rect: card.rect })),
      ...vue.ports.filter((port) => port.node).map((port, index) => ({
        key: `port-${port.node}-${port.side}-${index}`,
        rect: [port.dotRect[0] - 3, port.dotRect[1] - 3, port.dotRect[2] + 6, port.dotRect[3] + 6],
      })),
      ...vue.labels.map((label, index) => ({ key: `label-${index}`, rect: [label.rect[0] - 6, label.rect[1] - 6, label.rect[2] + 12, label.rect[3] + 12] })),
    ];
    const diff = await captureAndDiff(page, 'plain', regions);
    shots.plain = { native: shots.plain.native, vueflow: shots.plain.vueflow };
    const whole = diff.regions.find((item) => item.key === '__whole').raw;
    const cardDiffs = diff.regions.filter((item) => nMap.has(item.key));
    const portDiffs = diff.regions.filter((item) => item.key.startsWith('port-'));
    const worstCard = cardDiffs.reduce((acc, item) => (item.raw.mean > acc.raw.mean ? item : acc), cardDiffs[0]);
    const worstPort = portDiffs.reduce((acc, item) => (item.raw.mean > acc.raw.mean ? item : acc), portDiffs[0]);
    check('B 视觉：整画布平均像素差 ≤ 3/255', whole.mean <= 3, whole);
    check('B 视觉：连线方向与老画布一致（右出左进，无「底部出/顶部进」退化）', compareEdgeAnchors(native, vue).ok, compareEdgeAnchors(native, vue));
    check('B 视觉：逐卡片平均像素差 ≤ 3/255（100% 缩放、同视口、无重采样）', cardDiffs.length === 10 && cardDiffs.every((item) => item.raw.mean <= 3), {
      worst: worstCard && { key: worstCard.key, mean: Number(worstCard.raw.mean.toFixed(2)), big: Number((worstCard.raw.bigRatio * 100).toFixed(1)) },
    });
    // 锚点圆点的几何（bbox）比像素均值更能说明问题：16×16 区域里 1.5px 粗的圆环只要偏移
    // 0.4px，环边缘的抗锯齿像素就会差 40+，把区域均值抬高到 8/255 —— 那不是「样式不同」，
    // 是 SVG 圆 vs div 圆环在亚像素上的光栅化差。所以这里断言圆心/直径一致，并单独给证据。
    let worstDot = { key: null, delta: 0 };
    for (const port of vue.ports) {
      const peer = native.ports.find((item) => item.node === port.node && item.side === port.side);
      if (!peer) {
        worstDot = { key: `${port.node}-${port.side}`, delta: Infinity };
        continue;
      }
      const delta = maxDelta(port.dotRect, peer.dotRect);
      if (delta > worstDot.delta) worstDot = { key: `${port.node}-${port.side}`, delta };
    }
    check('锚点圆点几何与老画布一致（圆 bbox 四舍五入后 ≤1px：同圆心同直径）', worstDot.delta <= 1, worstDot);
    info.plainDiff = diff;
    await page.close();
  }

  // ==================== 场景 2：status（② 运行状态 / ④ taken 边 / ⑧ 长标签） ====================
  {
    const page = await openCase('status');
    const probed = await page.evaluate(probe);
    info.status = probed;
    const native = probed.native;
    const vue = probed.vueflow;
    const nMap = byKey(native.cards);
    const vMap = byKey(vue.cards);

    const expectTone = { n_appr: 'approved', n_cs: 'active', n_task: 'rejected', n_skip: 'pending', n_wait: 'pending' };
    const expectMark = { n_appr: '✓', n_cs: '', n_task: '✕', n_skip: '', n_wait: '' };
    const statusOk = Object.entries(expectTone).every(([key, tone]) => vMap.get(key)?.statusTone === tone && nMap.get(key)?.statusTone === tone);
    const markOk = Object.entries(expectMark).every(([key, mark]) => (vMap.get(key)?.statusText ?? '') === mark && (nMap.get(key)?.statusText ?? '') === mark);
    check('② 5 种状态的角标档位与老画布一致（approved/active/rejected/pending×2）', statusOk, {
      vue: Object.keys(expectTone).map((key) => `${key}:${vMap.get(key)?.statusTone}`),
      native: Object.keys(expectTone).map((key) => `${key}:${nMap.get(key)?.statusTone}`),
    });
    check('② 状态角标文案一致（✓ / 空 / ✕ / 空 / 空）', markOk, { vue: Object.keys(expectMark).map((key) => `${key}:${vMap.get(key)?.statusText}`) });
    check('② 无状态节点不画角标', vMap.get('n_none')?.statusText === null, vMap.get('n_none')?.statusText);
    const borderOk = [...nMap.keys()].every((key) => nMap.get(key).border === vMap.get(key)?.border && nMap.get(key).opacity === vMap.get(key)?.opacity);
    check('② 边框色 / 透明度逐节点一致（绿/蓝/红/灰淡化）', borderOk, {
      borders: [...nMap.entries()].map(([key, card]) => `${key}:${card.border}|${card.opacity}`),
      vue: [...vMap.entries()].map(([key, card]) => `${key}:${card.border}|${card.opacity}`),
    });
    check('② 状态角标底色一致（success / brand / error / placeholder）', [...nMap.keys()].every((key) => nMap.get(key).statusBg === vMap.get(key)?.statusBg), {
      vue: [...vMap.entries()].map(([key, card]) => `${key}:${card.statusBg}`),
    });
    const activePulse = vMap.get('n_cs')?.pulse;
    check('② ACTIVE 保留脉冲动画（::after animation 名以 flow-status-pulse 开头）', String(activePulse).startsWith('flow-status-pulse') && String(nMap.get('n_cs')?.pulse).startsWith('flow-status-pulse'), {
      vue: activePulse,
      native: nMap.get('n_cs')?.pulse,
    });
    check('② 副标题叠加运行状态（「按比例 50% · 处理中」/「需验收 · 已驳回」）', vMap.get('n_cs')?.sub === '按比例 50% · 处理中' && vMap.get('n_task')?.sub === '需验收 · 已驳回' && vMap.get('n_cs')?.sub === nMap.get('n_cs')?.sub, {
      vue: [vMap.get('n_cs')?.sub, vMap.get('n_task')?.sub],
      native: [nMap.get('n_cs')?.sub, nMap.get('n_task')?.sub],
    });

    // taken 边：3 条
    const takenVue = vue.lines.filter((line) => line.width > 2);
    const normalVue = vue.lines.filter((line) => line.width <= 2);
    check('④ taken 边加粗（2.4px）且其余 1.9px', takenVue.length === 3 && normalVue.length === vue.lines.length - 3 && normalVue.every((line) => Math.abs(line.width - 1.9) < 0.01), {
      taken: takenVue.map((line) => line.width),
      normal: [...new Set(normalVue.map((line) => line.width))],
    });
    check('④ taken 边用品牌色 + 老画布同色', takenVue.every((line) => line.stroke === native.lines.find((peer) => Math.abs(peer.width - line.width) < 0.01 && peer.stroke === line.stroke)?.stroke), {
      vue: [...new Set(takenVue.map((line) => line.stroke))],
      native: [...new Set(native.lines.map((line) => line.stroke))],
    });
    check('④ taken 边换箭头 marker（vf-flow-arrow-taken）', takenVue.every((line) => line.marker.includes('vf-flow-arrow-taken')) && normalVue.every((line) => line.marker.includes('vf-flow-arrow') && !line.marker.includes('taken')), {
      taken: takenVue[0]?.marker,
      normal: normalVue[0]?.marker,
    });
    check('⑧ 长标签「差异金额 < 10000（单级审批）」两端都在、位置一致', (() => {
      const a = vue.labels.find((item) => item.text === '差异金额 < 10000（单级审批）');
      const b = native.labels.find((item) => item.text === '差异金额 < 10000（单级审批）');
      return Boolean(a && b) && maxDelta(a.rect, b.rect) <= 0.6;
    })(), { vue: vue.labels.map((item) => item.text), native: native.labels.map((item) => item.text) });

    const regions = [
      { key: '__whole', rect: [0, 0, vue.wrap[0], vue.wrap[1]] },
      ...vue.cards.filter((card) => vMap.has(card.key)).map((card) => ({ key: card.key, rect: card.rect })),
    ];
    const diff = await captureAndDiff(page, 'status', regions);
    const whole = diff.regions.find((item) => item.key === '__whole').raw;
    const cardDiffs = diff.regions.filter((item) => vMap.has(item.key));
    const worst = cardDiffs.reduce((acc, item) => (item.raw.mean > acc.raw.mean ? item : acc), cardDiffs[0]);
    check('B 视觉（5 状态同屏）：整画布平均像素差 ≤ 3/255', whole.mean <= 3, whole);
    check('B 视觉（5 状态同屏）：逐卡片平均像素差 ≤ 3/255', cardDiffs.every((item) => item.raw.mean <= 3), {
      worst: worst && { key: worst.key, mean: Number(worst.raw.mean.toFixed(2)) },
    });
    info.statusDiff = diff;
    await page.close();
  }

  // ==================== 场景 3：issue（① 校验角标） ====================
  {
    const page = await openCase('issue');
    const probed = await page.evaluate(probe);
    info.issue = probed;
    const native = probed.native;
    const vue = probed.vueflow;
    const nMap = byKey(native.cards);
    const vMap = byKey(vue.cards);
    check('① 角标只出现在 issueNodeKeys 命中的节点上（appr1 / cs1）', vue.badges.length === 2 && vue.badges.map((item) => item.node).sort().join(',') === 'appr1,cs1', vue.badges);
    check('① 老画布同位置也有 2 个角标', native.badges.length === 2, native.badges.length);

    // 几何：圆心 = 卡片右上角往里 4px（老画布 circle(w-4, 4, r=8) → 16×16 盒子，右边/上边各外扩 4px）
    let worstBadge = 0;
    for (const badge of vue.badges) {
      const card = vMap.get(badge.node);
      if (!card) continue;
      const expected = [card.rect[0] + card.rect[2] - 12, card.rect[1] - 4, 16, 16];
      worstBadge = Math.max(worstBadge, maxDelta(badge.rect, expected));
      const peer = native.badges.find((item) => item.node === badge.node);
      if (peer) worstBadge = Math.max(worstBadge, maxDelta(badge.rect, peer.rect));
    }
    check('① 角标几何与老画布逐像素一致（16×16，右上角外扩 4px）', worstBadge <= 0.8, worstBadge);
    check('① 角标文案是 ! 且带悬停原因（title）', vue.badges.every((item) => item.text === '!' && (item.title || '').includes('校验')), vue.badges.map((item) => item.title));
    check('① 角标没被卡片 overflow 裁掉（宽度 16px 完整）', vue.badges.every((item) => Math.abs(item.rect[2] - 16) <= 0.6), vue.badges.map((item) => item.rect[2]));
    check('① 角标底色与老画布一致（error 色）', vue.badges.every((badge) => badge.bg === native.badges[0].bg), { vue: vue.badges[0]?.bg, native: native.badges[0]?.bg });
    check('① 命中节点的卡片虚线红框（has-issue）', vMap.get('appr1')?.border === nMap.get('appr1')?.border && vMap.get('appr1')?.border.includes('dashed'), vMap.get('appr1')?.border);

    const regions = [
      { key: '__whole', rect: [0, 0, vue.wrap[0], vue.wrap[1]] },
      ...vue.badges.map((badge) => {
        const card = vMap.get(badge.node);
        return { key: `badge-${badge.node}`, rect: [card.rect[0] + card.rect[2] - 18, card.rect[1] - 10, 30, 30] };
      }),
    ];
    const diff = await captureAndDiff(page, 'issue', regions);
    const badgeDiffs = diff.regions.filter((item) => item.key.startsWith('badge-'));
    check('B 视觉：角标区域平均像素差 ≤ 3/255', badgeDiffs.length === 2 && badgeDiffs.every((item) => item.raw.mean <= 3), badgeDiffs.map((item) => ({ key: item.key, mean: Number(item.raw.mean.toFixed(2)) })));
    info.issueDiff = diff;
    await page.close();
  }

  // ==================== 场景 4：readonly（③ 只读锚点 / ⑥ 只读交互） ====================
  {
    const page = await openCase('readonly');
    const probed = await page.evaluate(probe);
    info.readonly = probed;
    const native = probed.native;
    const vue = probed.vueflow;
    check('③ 只读：Handle 全部留在 DOM 里（20 个 = 10 节点 × 2）', vue.ports.length === 20 && native.ports.length === 0, {
      vue: vue.ports.length,
      native: native.ports.length,
      // 老画布只读态本来就不画锚点；新画布必须渲染但视觉隐藏（否则连线方向退化）
    });
    check('③ 只读：Handle 的 data-handlepos 是 left / right（不是兜底 top / bottom）', (() => {
      const positions = vue.ports.map((port) => port.handlePos);
      const lefts = positions.filter((value) => value === 'left').length;
      const rights = positions.filter((value) => value === 'right').length;
      return lefts === 10 && rights === 10;
    })(), [...new Set(vue.ports.map((port) => port.handlePos))]);
    check('③ 只读：圆点被视觉隐藏（opacity 0）但不吃指针', vue.ports.every((port) => port.opacity === '0' && port.pointer === 'none'), {
      opacity: [...new Set(vue.ports.map((port) => port.opacity))],
      pointer: [...new Set(vue.ports.map((port) => port.pointer))],
    });
    check('④ 只读：连线仍从右侧出、左侧进（边路径与老画布逐点一致）', (() => {
      const compared = compareEdgeAnchors(native, vue);
      return compared.ok && compared.count === 10;
    })(), compareEdgeAnchors(native, vue));
    check('⑥ 只读：没有「自动布局」按钮', vue.layoutActions === 0, vue.layoutActions);
    // 卡面外的追加修复（用户真机反馈）：只读态不该提示「拖出连线」——这时压根没有可拖的圆点
    check(
      '⑩ 只读态底部提示改成只读说明 + 引导另存草稿（不再出现「拖出连线」）',
      Boolean(vue.hint) && vue.hint.includes('只读模式') && vue.hint.includes('另存为新草稿') && !vue.hint.includes('拖出连线'),
      { vue: vue.hint },
    );
    check('⑩ 两套画布的只读提示文案逐字一致（老画布同一条也改了）', vue.hint === native.hint, { vue: vue.hint, native: native.hint });

    // ★ 截图必须在交互之前：只读态拖拽 = 平移画布，先拖再截会把视口挪走（整图差虚高）
    const diff = await captureAndDiff(page, 'readonly', [{ key: '__whole', rect: [0, 0, vue.wrap[0], vue.wrap[1]] }]);
    const whole = diff.regions[0].raw;
    check('B 视觉（只读）：整画布平均像素差 ≤ 3/255（圆点 opacity:0，与老画布「不画锚点」等效）', whole.mean <= 3, whole);
    info.readonlyDiff = diff;

    // 只读交互：拖节点不动、拉线不成边、Delete 无效
    await clearEvents(page);
    const card = vue.cards.find((item) => item.key === 'appr1');
    const paneBox = await (await page.$('[data-pane="vueflow"] .flow-canvas')).boundingBox();
    /** 卡片在「画布坐标」里的位置（只读下拖空白会平移视口，屏幕坐标会变，逻辑坐标不会） */
    const canvasPos = (row, key) => {
      const item = row.vueflow.cards.find((entry) => entry.key === key);
      return item ? [item.rect[0], item.rect[1]] : null;
    };
    await page.mouse.move(paneBox.x + card.rect[0] + 20, paneBox.y + card.rect[1] + 20);
    await page.mouse.down();
    for (let step = 1; step <= 5; step += 1) {
      await page.mouse.move(paneBox.x + card.rect[0] + 20 + step * 12, paneBox.y + card.rect[1] + 20 + step * 8);
      await page.waitForTimeout(25);
    }
    await page.mouse.up();
    await page.waitForTimeout(300);
    // 鼠标必须先挪开：卡片 hover 有 translateY(-2px)，不挪开会把悬停位移算进「画布坐标」
    await page.mouse.move(2, 2);
    await page.waitForTimeout(200);
    const afterDrag = await page.evaluate(probe);
    const moveEvents = (await events(page, 'vueflow')).filter((item) => item.event === 'move-node');
    /** 画布逻辑坐标 = (屏幕相对坐标 - 视口平移) / 缩放 */
    const logical = (row) => {
      const item = row.vueflow.cards.find((entry) => entry.key === 'appr1');
      if (!item || !row.vueflow.view) return null;
      return [
        Math.round(((item.rect[0] - row.vueflow.view.x) / row.vueflow.view.zoom) * 10) / 10,
        Math.round(((item.rect[1] - row.vueflow.view.y) / row.vueflow.view.zoom) * 10) / 10,
      ];
    };
    check('⑥ 只读：拖节点不产生 move-node 事件（只可能是平移视口）', moveEvents.length === 0, { moveEvents });
    check('⑥ 只读：节点画布坐标不变（拖的是画布，不是节点）', maxDelta(logical(probed), logical(afterDrag)) <= 0.6, {
      before: logical(probed),
      after: logical(afterDrag),
    });

    // 从出口圆点拉线（只读时应无效）
    const port = afterDrag.vueflow.ports.find((item) => item.node === 'appr1' && item.side === 'out');
    await page.mouse.move(paneBox.x + port.rect[0] + port.rect[2] / 2, paneBox.y + port.rect[1] + port.rect[3] / 2);
    await page.mouse.down();
    for (let step = 1; step <= 6; step += 1) {
      await page.mouse.move(paneBox.x + port.rect[0] + step * 20, paneBox.y + port.rect[1] + port.rect[3] / 2);
      await page.waitForTimeout(25);
    }
    await page.mouse.up();
    await page.waitForTimeout(300);
    const afterConnect = await page.evaluate(probe);
    const connectEvents = (await events(page, 'vueflow')).filter((item) => item.event === 'connect');
    check('⑥ 只读：从圆点拉不出线（边数不变、无 connect 事件）', afterConnect.vueflow.lines.length === probed.vueflow.lines.length && connectEvents.length === 0, {
      before: probed.vueflow.lines.length,
      after: afterConnect.vueflow.lines.length,
      connectEvents: connectEvents.length,
    });

    // Delete 无效
    await page.mouse.click(paneBox.x + card.rect[0] + 20, paneBox.y + card.rect[1] + 20);
    await page.waitForTimeout(200);
    await page.keyboard.press('Delete');
    await page.waitForTimeout(300);
    const afterDelete = await page.evaluate(probe);
    check('⑥ 只读：Delete 不删除节点', afterDelete.vueflow.cards.length === probed.vueflow.cards.length, { before: probed.vueflow.cards.length, after: afterDelete.vueflow.cards.length });
    await page.close();
  }

  // ==================== 场景 5：sub（⑪ 副标题 11 种情况） ====================
  {
    const page = await openCase('sub');
    const probed = await page.evaluate(probe);
    info.sub = probed;
    const native = probed.native;
    const vue = probed.vueflow;
    const vMap = byKey(vue.cards);
    const nMap = byKey(native.cards);
    const expected = {
      a1: '全部同意',
      a2: '或签',
      a3: '按比例 50%',
      b1: '未配置审批人',
      b2: '未配置负责人',
      c1: '需验收',
      c2: '生成任务',
      c3: '发通知（发完即通过）',
      c4: '分支网关',
      c5: '或分叉',
      c6: '只读知会',
      c7: '入口',
      c8: '出口',
      d1: '已通过',
      d2: '处理中',
    };
    const wrong = Object.entries(expected).filter(([key, text]) => vMap.get(key)?.sub !== text);
    check('⑪ 副标题 15 种情况文案逐条正确（会签3 / 缺配置2 / 类型说明6 / 状态2）', wrong.length === 0, wrong.map(([key, text]) => `${key}: 期望 ${text}，实际 ${vMap.get(key)?.sub}`));
    const drift = Object.keys(expected).filter((key) => nMap.get(key)?.sub !== vMap.get(key)?.sub);
    check('⑪ 与老画布逐字一致（同一份数据的副标题两边相同）', drift.length === 0, drift.map((key) => `${key}: 老 ${nMap.get(key)?.sub} / 新 ${vMap.get(key)?.sub}`));

    const diff = await captureAndDiff(page, 'sub', [
      { key: '__whole', rect: [0, 0, vue.wrap[0], vue.wrap[1]] },
      ...vue.cards.filter((card) => vMap.has(card.key)).map((card) => ({ key: card.key, rect: card.rect })),
    ]);
    const cards2 = diff.regions.filter((item) => vMap.has(item.key));
    const worst = cards2.reduce((acc, item) => (item.raw.mean > acc.raw.mean ? item : acc), cards2[0]);
    check('B 视觉（副标题全场景）：逐卡片平均像素差 ≤ 3/255', cards2.every((item) => item.raw.mean <= 3), {
      worst: worst && { key: worst.key, mean: Number(worst.raw.mean.toFixed(2)) },
    });
    info.subDiff = diff;
    await page.close();
  }

  // ==================== 场景 6：选中 / 键盘（⑤⑨，在比对台上验 emit 契约） ====================
  {
    const page = await openCase('plain');
    const vuePane = await page.$('[data-pane="vueflow"]');
    const paneBox = await vuePane.boundingBox();
    const probed = await page.evaluate(probe);
    const vMap = byKey(probed.vueflow.cards);
    const card = vMap.get('appr1');

    await clearEvents(page);
    await page.mouse.click(paneBox.x + card.rect[0] + 20, paneBox.y + card.rect[1] + 20);
    await page.waitForTimeout(250);
    const afterNodeClick = await events(page, 'vueflow');
    const selectedClass = await page.evaluate(() => Boolean(document.querySelector('[data-pane="vueflow"] .flow-node__card.is-selected')));
    check('⑤ 点节点 → emit select-node(key) 且卡片 is-selected', afterNodeClick.some((item) => item.event === 'select-node' && item.args === 'appr1') && selectedClass, {
      events: afterNodeClick,
      selectedClass,
    });
    // 选中态视觉：边框 + 外发光与老画布一致
    const selStyle = await page.evaluate(() => {
      const card = document.querySelector('[data-pane="vueflow"] .flow-node__card.is-selected');
      const cs = getComputedStyle(card);
      return { border: cs.borderColor, shadow: cs.boxShadow };
    });
    check('⑤ 选中态视觉：边框转类型色 + 2px 外发光', selStyle.border !== 'rgb(220, 226, 230)' && selStyle.shadow.includes('0px 0px 0px 2px'), selStyle);

    // 点边
    await clearEvents(page);
    const middle = await page.evaluate(() => {
      const path = document.querySelector('[data-pane="vueflow"] .flow-edge__hit');
      const point = path.getPointAtLength(path.getTotalLength() / 2);
      const ctm = path.getScreenCTM();
      return { x: point.x * ctm.a + point.y * ctm.c + ctm.e, y: point.x * ctm.b + point.y * ctm.d + ctm.f };
    });
    await page.mouse.click(middle.x, middle.y);
    await page.waitForTimeout(250);
    const afterEdgeClick = await events(page, 'vueflow');
    const edgeSelected = await page.evaluate(() => Boolean(document.querySelector('[data-pane="vueflow"] .flow-edge.is-selected')));
    check('⑤ 点连线 → emit select-edge(key) 且边 is-selected', afterEdgeClick.some((item) => item.event === 'select-edge' && typeof item.args === 'string') && edgeSelected, {
      events: afterEdgeClick,
      edgeSelected,
    });

    // 点空白
    await clearEvents(page);
    await page.mouse.click(paneBox.x + 600, paneBox.y + 420);
    await page.waitForTimeout(250);
    const afterBlank = await events(page, 'vueflow');
    check('⑤ 点空白 → select-node(null) + select-edge(null)（与老画布同一出口）', afterBlank.some((item) => item.event === 'select-node' && item.args === null) && afterBlank.some((item) => item.event === 'select-edge' && item.args === null), afterBlank);

    // Delete
    await page.mouse.click(paneBox.x + card.rect[0] + 20, paneBox.y + card.rect[1] + 20);
    await page.waitForTimeout(200);
    await clearEvents(page);
    await page.keyboard.press('Delete');
    await page.waitForTimeout(300);
    const afterDelete = await page.evaluate(probe);
    check('⑨ Delete 删除选中节点（emit delete-selected → 父组件删掉 → 画布少一个节点）', afterDelete.vueflow.cards.length === probed.vueflow.cards.length - 1, {
      before: probed.vueflow.cards.length,
      after: afterDelete.vueflow.cards.length,
      events: (await events(page, 'vueflow')).map((item) => item.event),
    });

    // Esc 取消选中
    await page.mouse.click(paneBox.x + (vMap.get('cs1')?.rect[0] ?? 0) + 20, paneBox.y + (vMap.get('cs1')?.rect[1] ?? 0) + 20);
    await page.waitForTimeout(200);
    await clearEvents(page);
    await page.keyboard.press('Escape');
    await page.waitForTimeout(250);
    const afterEsc = await events(page, 'vueflow');
    const stillSelected = await page.evaluate(() => Boolean(document.querySelector('[data-pane="vueflow"] .flow-node__card.is-selected')));
    check('⑨ Esc 取消选中（select-node(null) + select-edge(null)，视觉取消）', afterEsc.some((item) => item.event === 'select-node' && item.args === null) && !stillSelected, { events: afterEsc, stillSelected });

    // 空格 + 拖节点 = 平移画布
    const beforePan = await page.evaluate(() => {
      const pane = document.querySelector('[data-pane="vueflow"] .vue-flow__transformationpane');
      const matrix = getComputedStyle(pane).transform;
      const nums = matrix.slice(7, -1).split(',').map(Number);
      return { x: nums[4], y: nums[5] };
    });
    const target = vMap.get('appr1');
    await page.mouse.move(paneBox.x + target.rect[0] + 20, paneBox.y + target.rect[1] + 20);
    await page.keyboard.down('Space');
    await page.mouse.down();
    for (let step = 1; step <= 6; step += 1) {
      await page.mouse.move(paneBox.x + target.rect[0] + 20 + step * 10, paneBox.y + target.rect[1] + 20 + step * 5);
      await page.waitForTimeout(30);
    }
    await page.mouse.up();
    await page.keyboard.up('Space');
    await page.waitForTimeout(250);
    const afterPan = await page.evaluate(() => {
      const pane = document.querySelector('[data-pane="vueflow"] .vue-flow__transformationpane');
      const matrix = getComputedStyle(pane).transform;
      const nums = matrix.slice(7, -1).split(',').map(Number);
      return { x: nums[4], y: nums[5] };
    });
    check('⑨ 空格 + 在节点上拖拽 = 平移画布（节点不移动）', Math.abs(afterPan.x - beforePan.x - 60) < 6 && Math.abs(afterPan.y - beforePan.y - 30) < 6, { before: beforePan, after: afterPan });
    await page.close();
  }

  // ==================== 阶段 B：真实编辑器页（⑦ 拖拽创建 / 交互等价） ====================
  if (DEF_ID) {
    /** 登录取 token（浏览器路由守卫读 localStorage.x_access_token）；缓存 8 分钟避免限流 */
    const cacheFile = '/tmp/vueflow_verify_token';
    let token = null;
    try {
      const cached = fs.readFileSync(cacheFile, 'utf8').trim().split('\n');
      if (cached[0] && Date.now() - Number(cached[1]) < 8 * 60 * 1000) token = cached[0];
    } catch {
      /* 无缓存 */
    }
    if (!token) {
      const resp = await fetch(`${BASE}/api/system/auth/login`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ username: USER, password: PASS }),
      });
      const body = await resp.json();
      token = body?.data?.access_token;
      if (!token) throw new Error('登录失败：' + JSON.stringify(body).slice(0, 200));
      fs.writeFileSync(cacheFile, `${token}\n${Date.now()}`);
    }
    await context.addInitScript((value) => {
      localStorage.setItem('x_access_token', value);
      localStorage.setItem('cp-onboarding-done', '1');
      sessionStorage.setItem('login-unread-shown', '1');
    }, token);

    /** 打开编辑器页（指定引擎），等画布就绪 */
    async function openEditor(engineName) {
      const page = await context.newPage();
      page.on('console', (message) => {
        if (message.type() === 'error') consoleErrors.editor.push(`${engineName}: ${message.text()}`);
      });
      page.on('pageerror', (error) => consoleErrors.editor.push(`${engineName}: PAGEERROR ${error.message}`));
      await page.goto(`${BASE}/flow/def/${DEF_ID}/edit?canvas=${engineName}`, { waitUntil: 'domcontentloaded' });
      await page.waitForSelector('.flow-node__card', { timeout: 30000 });
      // 等 fitView 落地（默认 100% 会先闪一下）
      for (let attempt = 0; attempt < 30; attempt += 1) {
        const zoom = await page.evaluate(() => {
          const pane = document.querySelector('.vue-flow__transformationpane') || document.querySelector('.flow-canvas__svg');
          const matrix = pane ? getComputedStyle(pane).transform : 'none';
          return matrix.startsWith('matrix(') ? Number(matrix.slice(7, -1).split(',')[0]) : 0;
        });
        if (zoom > 0 && zoom < 0.999) break;
        await page.waitForTimeout(120);
      }
      await page.waitForTimeout(600);
      return page;
    }

    /** 编辑器页几何探针 */
    const editorProbe = () => {
      const round = (v) => Math.round(v * 10) / 10;
      const wrap = document.querySelector('.flow-canvas');
      const wr = wrap.getBoundingClientRect();
      const pane = document.querySelector('.vue-flow__transformationpane') || document.querySelector('.flow-canvas__svg');
      const matrix = getComputedStyle(pane).transform;
      const nums = matrix.startsWith('matrix(') ? matrix.slice(7, -1).split(',').map(Number) : [1, 0, 0, 1, 0, 0];
      return {
        view: { zoom: Math.round(nums[0] * 1000) / 1000, x: round(nums[4]), y: round(nums[5]) },
        wrap: { left: round(wr.left), top: round(wr.top), width: round(wr.width), height: round(wr.height) },
        cards: [...document.querySelectorAll('.flow-node__card')].map((card) => {
          const rect = card.getBoundingClientRect();
          return {
            key: card.getAttribute('data-node-key') || card.closest('[data-node-key]')?.getAttribute('data-node-key') || null,
            rect: { left: round(rect.left), top: round(rect.top), width: round(rect.width), height: round(rect.height) },
            sub: card.querySelector('.flow-node__sub')?.textContent ?? null,
            selected: card.className.includes('is-selected'),
          };
        }),
      };
    };

    const results = {};
    for (const engineName of ['native', 'vueflow']) {
      const page = await openEditor(engineName);
      const before = await page.evaluate(editorProbe);
      // ⑦ 从左侧面板「审批」拖到画布某点（画布坐标由屏幕坐标反算，两套画布应当落在同一位置）
      const palette = await page.$('.palette-item[data-node-type="APPROVAL"]');
      const paletteBox = await palette.boundingBox();
      const targetScreen = { x: before.wrap.left + before.wrap.width * 0.5, y: before.wrap.top + before.wrap.height * 0.62 };
      await page.mouse.move(paletteBox.x + paletteBox.width / 2, paletteBox.y + paletteBox.height / 2);
      await page.mouse.down();
      for (let step = 1; step <= 8; step += 1) {
        await page.mouse.move(paletteBox.x + ((targetScreen.x - paletteBox.x) / 8) * step, paletteBox.y + ((targetScreen.y - paletteBox.y) / 8) * step);
        await page.waitForTimeout(25);
      }
      const ghost = await page.evaluate(() => Boolean(document.querySelector('.flow-editor__ghost')));
      await page.mouse.up();
      await page.waitForTimeout(600);
      const after = await page.evaluate(editorProbe);
      const created = after.cards.find((card) => !before.cards.some((old) => old.key === card.key));
      results[engineName] = { before, after, created, ghost, targetScreen };
      check(`⑦ [${engineName}] 拖入时显示预览幽灵（跟随鼠标）`, ghost, ghost);
      check(`⑦ [${engineName}] 从左侧面板拖「审批」到画布 → 创建节点`, Boolean(created), { cards: after.cards.length, before: before.cards.length });
      if (created) {
        // 落点断言：节点盒子中心 = 松手点（画布坐标），换算回屏幕后与松手点误差 < 2px
        const center = {
          x: created.rect.left + created.rect.width / 2,
          y: created.rect.top + created.rect.height / 2,
        };
        const delta = Math.hypot(center.x - targetScreen.x, center.y - targetScreen.y);
        check(`⑦ [${engineName}] 新节点落在松手位置（中心偏差 ≤ 2px）`, delta <= 2, { delta: Math.round(delta * 100) / 100, center, targetScreen });
      }
      await page.screenshot({ path: path.join(SHOT_DIR, `editor-${engineName}-created.png`) });
      await page.close();
    }
    if (results.native.created && results.vueflow.created) {
      // 两套画布的落点必须等价：新节点的「屏幕位置 - 画布原点偏移」一致
      const toCanvas = (row) => ({
        x: (row.created.rect.left - row.after.wrap.left - row.after.view.x) / row.after.view.zoom,
        y: (row.created.rect.top - row.after.wrap.top - row.after.view.y) / row.after.view.zoom,
      });
      const a = toCanvas(results.native);
      const b = toCanvas(results.vueflow);
      const delta = Math.hypot(a.x - b.x, a.y - b.y);
      check('⑦ 交互等价：两套画布创建的节点画布坐标一致（≤2px）', delta <= 2, { native: a, vueflow: b, delta: Math.round(delta * 100) / 100 });
    }
    info.editor = {
      native: { ghost: results.native.ghost, createdKey: results.native.created?.key ?? null },
      vueflow: { ghost: results.vueflow.ghost, createdKey: results.vueflow.created?.key ?? null },
    };
  } else {
    console.log('（跳过阶段 B：未给 --def-id）');
  }

  // ==================== 控制台 ====================
  // 判定口径与 VUE-FLOW-1 一致：只要求新画布**不引入新报错**。
  // 页面本来就有一条 `notice/stream?token=` 401（后端认证层不吃查询令牌，全站既有缺陷），
  // 两套画布都会出现，不算本卡引入。
  const nativeErrors = new Set(consoleErrors.editor.filter((line) => line.startsWith('native:')).map((line) => line.replace(/^native: /, '')));
  const newErrors = consoleErrors.editor
    .filter((line) => line.startsWith('vueflow:'))
    .map((line) => line.replace(/^vueflow: /, ''))
    .filter((message) => !nativeErrors.has(message) && !message.includes('401'));
  check('控制台 0 新增报错（与老画布同集合；全站既有的 notice/stream 401 除外）', consoleErrors.harness.length === 0 && newErrors.length === 0, {
    harness: consoleErrors.harness,
    newErrors,
    nativeErrors: [...nativeErrors],
  });

  const report = {
    generatedAt: new Date().toISOString(),
    baseUrl: BASE,
    browser: BROWSER,
    defId: DEF_ID || null,
    passed: checks.filter((item) => item.ok).length,
    failed: checks.filter((item) => !item.ok).length,
    checks,
    consoleErrors,
    shots,
    info,
  };
  fs.mkdirSync(path.dirname(OUT), { recursive: true });
  fs.writeFileSync(OUT, JSON.stringify(report, null, 2));
  console.log(`\n${report.passed} passed / ${report.failed} failed · 报告 ${OUT}`);
  await browser.close();
  process.exit(report.failed === 0 ? 0 : 1);
})().catch((error) => {
  console.error(error);
  process.exit(2);
});
