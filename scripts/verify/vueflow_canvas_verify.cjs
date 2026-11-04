#!/usr/bin/env node
/**
 * VUE-FLOW-1 真机验证：Vue Flow 画布基座 vs 老画布（同一份图、同一台浏览器）。
 *
 * 一条命令跑完全部验收：
 *   · 9 种节点渲染（尺寸 / 圆角 / 色条 / 图标底块 / 文案）
 *   · 连线（正交圆角 + 箭头 + 条件标签）
 *   · 滚轮缩放（**以鼠标位置为锚点**，不是画布中心）
 *   · 拖空白平移 / 拖节点（落 10px 网格）/ 拉线成边 / 工具栏 / 小地图 / fitView
 *   · 控制台 0 报错
 *   · 两套画布同图截图 + 逐节点像素差（验收 C）
 *
 * # 用法
 *
 *   node scripts/verify/vueflow_canvas_verify.cjs \
 *     --base-url http://127.0.0.1:5188 \
 *     --def-id <流程定义 id> --user admin --password 'Admin@123456' \
 *     --shot-dir docs/evidence/20261104-vueflow-canvas/shots \
 *     --out docs/evidence/20261104-vueflow-canvas/report.json
 *
 * 退出码：0 = 全部通过；1 = 有用例失败（失败项写在 JSON 的 `checks` 里）。
 */
'use strict';

const fs = require('node:fs');
const path = require('node:path');

/** 从脚本目录向上找 Playwright（本仓 frontend/node_modules 里有一份） */
function loadPlaywright() {
  const candidates = ['@playwright/test', path.join(__dirname, '..', '..', 'frontend', 'node_modules', '@playwright/test'), '/opt/cloudpivot/frontend/node_modules/@playwright/test', 'playwright'];
  for (const candidate of candidates) {
    try {
      return require(candidate);
    } catch {
      /* 换下一个 */
    }
  }
  throw new Error('找不到 Playwright；请在仓库内运行（frontend/node_modules 里有一份）');
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
/** 只读验证用的「已发布」流程 id（可选；不给就跳过只读用例） */
const READONLY_DEF_ID = args['readonly-def-id'] || '';
const USER = args.user || 'admin';
const PASS = args.password || 'Admin@123456';
const SHOT_DIR = args['shot-dir'] || 'docs/evidence/20261104-vueflow-canvas/shots';
const OUT = args.out || 'docs/evidence/20261104-vueflow-canvas/report.json';
const BROWSER = (args.browser || 'chromium').toLowerCase();

if (!DEF_ID) {
  console.error('缺少 --def-id');
  process.exit(64);
}

/** 采集结果（每条：name / ok / detail） */
const checks = [];
function check(name, ok, detail) {
  checks.push({ name, ok: Boolean(ok), detail: detail === undefined ? null : detail });
  console.log(`${ok ? 'PASS' : 'FAIL'} · ${name}${detail === undefined || ok ? '' : ' → ' + JSON.stringify(detail)}`);
}

/** 登录拿 token（浏览器里路由守卫读 localStorage.x_access_token）。
 *
 * 后端对登录有频率限制（10005「登录尝试过于频繁」），所以：
 * 1. 本地缓存 8 分钟内的 token（`/tmp/vueflow_verify_token`），反复跑不再重复登录；
 * 2. 命中限流时退避重试（最多 6 次，间隔 15s）。
 */
async function login() {
  const cacheFile = '/tmp/vueflow_verify_token';
  try {
    const cached = fs.readFileSync(cacheFile, 'utf8').trim();
    const [token, stamp] = cached.split('\n');
    if (token && stamp && Date.now() - Number(stamp) < 8 * 60 * 1000) {
      return token;
    }
  } catch {
    /* 没缓存，正常登录 */
  }
  for (let attempt = 1; attempt <= 6; attempt += 1) {
    const resp = await fetch(`${BASE}/api/system/auth/login`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username: USER, password: PASS }),
    });
    const body = await resp.json();
    const token = body && body.data && body.data.access_token;
    if (token) {
      fs.writeFileSync(cacheFile, `${token}\n${Date.now()}`);
      return token;
    }
    if (body && body.code === 10005) {
      console.log(`登录限流，${15}s 后重试（第 ${attempt}/6 次）`);
      await new Promise((resolve) => setTimeout(resolve, 15000));
      continue;
    }
    throw new Error('登录失败：' + JSON.stringify(body).slice(0, 200));
  }
  throw new Error('登录失败：连续 6 次被限流');
}

/** 页面内探针：视口变换 + 每张卡片的几何/样式（两套画布通用） */
const probe = () => {
  const round = (v) => Math.round(v * 10) / 10;
  const wrap = document.querySelector('.flow-canvas');
  const wrapRect = wrap ? wrap.getBoundingClientRect() : null;
  // 视口变换：新画布在 .vue-flow__transformationpane，老画布在 .flow-canvas__svg
  const pane = document.querySelector('.vue-flow__transformationpane') || document.querySelector('.flow-canvas__svg');
  const matrix = pane ? getComputedStyle(pane).transform : 'none';
  const nums = matrix.startsWith('matrix(') ? matrix.slice(7, -1).split(',').map(Number) : null;
  const view = nums ? { zoom: Math.round(nums[0] * 1000) / 1000, x: round(nums[4]), y: round(nums[5]) } : null;
  const cards = [...document.querySelectorAll('.flow-node__card')].map((card) => {
    const rect = card.getBoundingClientRect();
    const style = getComputedStyle(card);
    const icon = card.querySelector('.flow-node__icon');
    const bar = card.querySelector('.flow-node__bar');
    const title = card.querySelector('.flow-node__title');
    const sub = card.querySelector('.flow-node__sub');
    return {
      // 老画布把 data-node-key 挂在 <g class="flow-node"> 上，新画布挂在卡片自己身上 → 两处都认
      key: card.getAttribute('data-node-key') || card.closest('[data-node-key]')?.getAttribute('data-node-key') || card.parentElement?.getAttribute('data-node-key') || null,
      type: card.getAttribute('data-node-type') || card.closest('[data-node-type]')?.getAttribute('data-node-type') || card.parentElement?.getAttribute('data-node-type') || null,
      rect: { left: round(rect.left), top: round(rect.top), width: round(rect.width), height: round(rect.height) },
      radius: style.borderRadius,
      padding: style.padding,
      shadow: style.boxShadow !== 'none',
      barWidth: bar ? round(bar.getBoundingClientRect().width) : null,
      icon: icon
        ? { w: round(icon.getBoundingClientRect().width), h: round(icon.getBoundingClientRect().height), text: icon.textContent, bg: getComputedStyle(icon).backgroundColor }
        : null,
      title: title ? { text: title.textContent, size: getComputedStyle(title).fontSize, weight: getComputedStyle(title).fontWeight } : null,
      sub: sub ? { text: sub.textContent, size: getComputedStyle(sub).fontSize } : null,
      status: card.querySelector('.flow-node__status') ? card.querySelector('.flow-node__status').textContent : null,
    };
  });
  const lines = [...document.querySelectorAll('.flow-edge__line')].map((p) => ({
    d: p.getAttribute('d'),
    stroke: getComputedStyle(p).stroke,
    width: getComputedStyle(p).strokeWidth,
    dash: getComputedStyle(p).strokeDasharray,
    marker: p.getAttribute('marker-end'),
  }));
  const labels = [...document.querySelectorAll('.flow-edge__label text')].map((t) => t.textContent);
  return {
    view,
    wrapRect: wrapRect ? { left: round(wrapRect.left), top: round(wrapRect.top), width: round(wrapRect.width), height: round(wrapRect.height) } : null,
    cards,
    lines,
    labels,
    minimap: Boolean(document.querySelector('.flow-canvas__minimap')),
    toolbar: {
      zoomButtons: document.querySelectorAll('.flow-canvas__zoom-btn').length,
      layoutActions: document.querySelectorAll('.flow-canvas__action').length,
      percentText: (document.querySelector('.flow-canvas__zoom-btn--scale') || {}).textContent?.trim() || null,
    },
    engine: document.querySelector('.vue-flow') ? 'vueflow' : 'native',
  };
};

(async () => {
  const playwright = loadPlaywright();
  const engine = playwright[BROWSER];
  if (!engine) throw new Error(`不支持的浏览器：${BROWSER}`);
  const token = await login();
  fs.mkdirSync(SHOT_DIR, { recursive: true });

  const browser = await engine.launch();
  const context = await browser.newContext({ viewport: { width: 1600, height: 1000 }, deviceScaleFactor: 1 });
  await context.addInitScript((value) => {
    localStorage.setItem('x_access_token', value);
    localStorage.setItem('cp-onboarding-done', '1');
    // 登录后的未读汇总弹窗会污染截图（同一份代码两次跑差 6000 像素），按掉
    sessionStorage.setItem('login-unread-shown', '1');
  }, token);

  /** 控制台报错按引擎分开收集（要能区分「我的改动引入的」和「页面本来就有的」） */
  const consoleErrors = { vueflow: [], native: [], other: [] };
  const pageErrors = { vueflow: [], native: [], other: [] };
  /** HTTP >= 400 的响应（控制台里那句 "Failed to load resource: 401" 的出处） */
  const failedResponses = { vueflow: [], native: [], other: [] };

  /** 打开某个引擎的编辑器页，等画布就绪；返回 { page, visibleMs } */
  async function open(engineName) {
    const page = await context.newPage();
    const bucket = engineName === 'vueflow' || engineName === 'native' ? engineName : 'other';
    page.on('console', (message) => {
      if (message.type() === 'error') consoleErrors[bucket].push(message.text());
    });
    page.on('pageerror', (error) => pageErrors[bucket].push(error.message));
    page.on('response', (response) => {
      if (response.status() >= 400) {
        const path = response.url().replace(BASE, '');
        failedResponses[bucket].push(`${response.status()} ${path}`);
      }
    });
    const started = Date.now();
    await page.goto(`${BASE}/flow/def/${DEF_ID}/edit?canvas=${engineName}`, { waitUntil: 'domcontentloaded' });
    await page.waitForSelector('.flow-canvas', { timeout: 30000 });
    // 新画布是异步组件：等卡片真正画出来（= 画布可见）
    await page.waitForSelector('.flow-node__card', { timeout: 30000 });
    const visibleMs = Date.now() - started;
    // 等 fitView 真正落到视口上（默认视口是 100%，fit 后必然 < 100%）
    for (let attempt = 0; attempt < 30; attempt += 1) {
      const zoom = await page.evaluate(() => {
        const pane = document.querySelector('.vue-flow__transformationpane') || document.querySelector('.flow-canvas__svg');
        const matrix = pane ? getComputedStyle(pane).transform : 'none';
        return matrix.startsWith('matrix(') ? Number(matrix.slice(7, -1).split(',')[0]) : 0;
      });
      if (zoom > 0 && zoom < 0.999) {
        break;
      }
      await page.waitForTimeout(120);
    }
    await page.waitForTimeout(700);
    return { page, visibleMs };
  }

  // ==================== ① Vue Flow 画布 ====================
  const openedVue = await open('vueflow');
  const page = openedVue.page;
  const canvasVisibleMs = openedVue.visibleMs;
  check('新画布 DOMContentLoaded → 卡片可见 < 3000ms', canvasVisibleMs < 3000, canvasVisibleMs);

  const vue = await page.evaluate(probe);
  const canvasEl = await page.$('.flow-canvas');
  await canvasEl.screenshot({ path: path.join(SHOT_DIR, 'vueflow.png') });

  // 100% 缩放下再截一张：两套画布的 resetView 都是 {x:40,y:40,zoom:1}，
  // 于是同一节点的屏幕矩形**完全一致**，可以逐像素 1:1 比对（不涉及重采样）。
  await page.click('.flow-canvas__zoom-btn[data-zoom="reset"]');
  await page.waitForTimeout(400);
  const vue100 = await page.evaluate(probe);
  await canvasEl.screenshot({ path: path.join(SHOT_DIR, 'vueflow-100.png') });
  await page.click('.flow-canvas__zoom-btn[data-zoom="fit"]');
  await page.waitForTimeout(400);

  check('新画布生效（页面上确实是 .vue-flow）', vue.engine === 'vueflow', vue.engine);
  check('节点数量 10（9 种类型各一 + 任务节点）', vue.cards.length === 10, vue.cards.length);
  const types = [...new Set(vue.cards.map((card) => card.type))].sort();
  check('9 种节点类型全部渲染', types.length === 9, types);
  check(
    '卡片尺寸：审批 150×64 / 开始结束 96×44 / 条件并行 132×84 / 抄送通知 140×60',
    ['APPROVAL', 'START', 'CONDITION', 'CC'].every((type) => {
      const card = vue.cards.find((item) => item.type === type);
      const expect = { APPROVAL: [150, 64], START: [96, 44], CONDITION: [132, 84], CC: [140, 60] }[type];
      return card && Math.abs(card.rect.width / vue.view.zoom - expect[0]) < 1.5 && Math.abs(card.rect.height / vue.view.zoom - expect[1]) < 1.5;
    }),
    vue.cards.map((c) => `${c.type}:${c.rect.width}x${c.rect.height}`),
  );
  check('卡片圆角 11px', vue.cards.every((card) => card.radius === '11px'), vue.cards.map((c) => c.radius));
  check('左侧色条 3.5px', vue.cards.filter((card) => card.type !== 'START' && card.type !== 'END').every((card) => Math.abs(card.barWidth - 3.5 * vue.view.zoom) < 0.6), vue.cards.map((c) => c.barWidth));
  check('图标底块 30×30（紧凑排版 22×22）', vue.cards.every((card) => (card.type === 'START' || card.type === 'END' ? Math.abs(card.icon.w - 22 * vue.view.zoom) < 1.2 : Math.abs(card.icon.w - 30 * vue.view.zoom) < 1.2)), vue.cards.map((c) => `${c.type}:${c.icon.w}`));
  check('卡片投影存在', vue.cards.every((card) => card.shadow), null);
  check('连线数量 10 + 条件标签 3 条', vue.lines.length === 10 && vue.labels.length === 3, { lines: vue.lines.length, labels: vue.labels });
  // 同层直连是纯水平线（没有拐角 → 没有 Q），有拐角的边才该出现 Q
  check(
    '连线是正交折线（全部 L，拐角处有 Q 圆角）',
    vue.lines.every((line) => line.d.includes('L')) && vue.lines.filter((line) => line.d.includes('Q')).length >= 5,
    { total: vue.lines.length, withArc: vue.lines.filter((line) => line.d.includes('Q')).length },
  );
  check('连线有箭头 marker', vue.lines.every((line) => (line.marker || '').includes('vf-flow-arrow')), vue.lines[0] && vue.lines[0].marker);
  check('兜底分支是虚线', vue.lines.some((line) => line.dash && line.dash !== 'none'), vue.lines.map((l) => l.dash));
  check('小地图显示（showMinimap=true）', vue.minimap, null);
  check('缩放工具栏 4 个按钮 + 自动布局按钮', vue.toolbar.zoomButtons === 4 && vue.toolbar.layoutActions >= 1, vue.toolbar);
  check('缩放百分比读数正常', /^\d+%$/.test(vue.toolbar.percentText || ''), vue.toolbar.percentText);

  // ---------- 滚轮缩放：以鼠标位置为锚点 ----------
  const anchorCard = vue.cards.find((card) => card.key === 'appr1');
  const anchorX = anchorCard.rect.left + anchorCard.rect.width / 2;
  const anchorY = anchorCard.rect.top + anchorCard.rect.height / 2;
  const zoomBefore = vue.view.zoom;
  await page.mouse.move(anchorX, anchorY);
  await page.mouse.wheel(0, -300);
  await page.waitForTimeout(350);
  const afterWheel = await page.evaluate(probe);
  const anchorAfter = afterWheel.cards.find((card) => card.key === 'appr1');
  const drift = Math.hypot(anchorAfter.rect.left + anchorAfter.rect.width / 2 - anchorX, anchorAfter.rect.top + anchorAfter.rect.height / 2 - anchorY);
  check('滚轮能放大', afterWheel.view.zoom > zoomBefore + 0.01, { before: zoomBefore, after: afterWheel.view.zoom });
  check('缩放以鼠标位置为锚点（锚点卡片漂移 < 4px）', drift < 4, { drift: Math.round(drift * 10) / 10 });

  // ---------- 工具栏：适应画布 + 缩放按钮 ----------
  await page.click('.flow-canvas__zoom-btn[data-zoom="fit"]');
  await page.waitForTimeout(400);
  const afterFit = await page.evaluate(probe);
  check('工具栏「适应画布」可用（视口被重算）', afterFit.view.zoom !== afterWheel.view.zoom, { before: afterWheel.view.zoom, after: afterFit.view.zoom });
  await page.click('.flow-canvas__zoom-btn[data-zoom="in"]');
  await page.waitForTimeout(300);
  const afterIn = await page.evaluate(probe);
  check('工具栏「放大」可用', afterIn.view.zoom > afterFit.view.zoom, { before: afterFit.view.zoom, after: afterIn.view.zoom });
  await page.click('.flow-canvas__zoom-btn[data-zoom="reset"]');
  await page.waitForTimeout(300);
  const afterReset = await page.evaluate(probe);
  check('点百分比回到 100%', Math.abs(afterReset.view.zoom - 1) < 0.01, afterReset.view.zoom);
  await page.click('.flow-canvas__zoom-btn[data-zoom="fit"]');
  await page.waitForTimeout(400);

  // ---------- 平移：拖空白处 ----------
  const beforePan = await page.evaluate(probe);
  // 起点要避开小地图（左下 150×96）与工具栏（右下），否则拖到的是小地图/按钮
  const panStart = { x: beforePan.wrapRect.left + beforePan.wrapRect.width * 0.45, y: beforePan.wrapRect.top + 90 };
  await page.mouse.move(panStart.x, panStart.y);
  await page.mouse.down();
  for (let step = 1; step <= 6; step += 1) {
    await page.mouse.move(panStart.x + step * 12, panStart.y - step * 6);
    await page.waitForTimeout(30);
  }
  await page.mouse.up();
  await page.waitForTimeout(300);
  const afterPan = await page.evaluate(probe);
  const panDx = afterPan.view.x - beforePan.view.x;
  const panDy = afterPan.view.y - beforePan.view.y;
  const panCardDx = afterPan.cards[0].rect.left - beforePan.cards[0].rect.left;
  check('拖空白处能平移（视口位移 ≈ 拖拽距离，节点同步位移）', Math.abs(panDx - 72) < 6 && Math.abs(panDy + 36) < 6 && Math.abs(panCardDx - panDx) < 2, { panDx, panDy, panCardDx });

  // ---------- 拖节点：落 10px 网格 ----------
  await page.click('.flow-canvas__zoom-btn[data-zoom="fit"]');
  await page.waitForTimeout(400);
  // 鼠标必须先挪开：卡片 hover 会 translateY(-2px)，不挪开会把「悬停位移」算进拖拽量
  await page.mouse.move(5, 5);
  await page.waitForTimeout(200);
  const beforeDrag = await page.evaluate(probe);
  const dragCard = beforeDrag.cards.find((card) => card.key === 'appr1');
  const dragFrom = { x: dragCard.rect.left + dragCard.rect.width / 2, y: dragCard.rect.top + dragCard.rect.height / 2 };
  const ZOOM = beforeDrag.view.zoom;
  // 故意拖一个「不是整格」的位移：23px 屏幕像素 → 画布 23/zoom → 应吸附到最近的 10 的倍数
  const movePx = 23;
  await page.mouse.move(dragFrom.x, dragFrom.y);
  await page.mouse.down();
  for (let step = 1; step <= 5; step += 1) {
    await page.mouse.move(dragFrom.x + (movePx / 5) * step, dragFrom.y + (movePx / 5) * step);
    await page.waitForTimeout(30);
  }
  await page.mouse.up();
  await page.waitForTimeout(400);
  await page.mouse.move(5, 5);
  await page.waitForTimeout(200);
  const afterDrag = await page.evaluate(probe);
  const movedCard = afterDrag.cards.find((card) => card.key === 'appr1');
  const canvasDx = (movedCard.rect.left - dragCard.rect.left) / ZOOM;
  const canvasDy = (movedCard.rect.top - dragCard.rect.top) / ZOOM;
  const expected = Math.round(movePx / ZOOM / 10) * 10;
  check('拖节点能移动', Math.abs(canvasDx) > 1 || Math.abs(canvasDy) > 1, { canvasDx, canvasDy });
  check('落点吸附 10px 网格（23px 拖动吸附成整格）', Math.abs(canvasDx - expected) < 0.6 && Math.abs(canvasDy - expected) < 0.6, { canvasDx, canvasDy, expected });

  // ---------- 拉线：从 appr1 右侧句柄拖到 cc1 左侧句柄 ----------
  const edgesBefore = (await page.evaluate(probe)).lines.length;
  const srcHandle = await page.$('.vue-flow__node[data-id="appr1"] .vue-flow__handle-right');
  const dstNode = await page.$('.vue-flow__node[data-id="cc1"]');
  if (srcHandle && dstNode) {
    const box = await srcHandle.boundingBox();
    const target = await dstNode.boundingBox();
    await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
    await page.mouse.down();
    for (let step = 1; step <= 8; step += 1) {
      await page.mouse.move(box.x + ((target.x + 2 - box.x) / 8) * step, box.y + ((target.y + target.height / 2 - box.y) / 8) * step);
      await page.waitForTimeout(25);
    }
    await page.mouse.up();
    await page.waitForTimeout(500);
    const edgesAfter = (await page.evaluate(probe)).lines.length;
    check('从右侧 Handle 拖到左侧 Handle 能建成连线', edgesAfter === edgesBefore + 1, { before: edgesBefore, after: edgesAfter });
  } else {
    check('从右侧 Handle 拖到左侧 Handle 能建成连线', false, '找不到句柄节点');
  }

  // ---------- 删除：选中节点 + Delete ----------
  const nodesBefore = (await page.evaluate(probe)).cards.length;
  const delCard = (await page.evaluate(probe)).cards.find((card) => card.key === 'cc1');
  await page.mouse.click(delCard.rect.left + delCard.rect.width / 2, delCard.rect.top + delCard.rect.height / 2);
  await page.waitForTimeout(200);
  const selected = await page.evaluate(() => Boolean(document.querySelector('.flow-node__card.is-selected')));
  check('点节点能选中（is-selected）', selected, null);
  await page.keyboard.press('Delete');
  await page.waitForTimeout(400);
  const nodesAfter = (await page.evaluate(probe)).cards.length;
  check('Delete 删除选中节点（事件到父组件生效）', nodesAfter === nodesBefore - 1, { before: nodesBefore, after: nodesAfter });

  // ---------- 小地图拖拽（拖一下即可平移视口；Vue Flow 的小地图是「拖」优先） ----------
  const miniBox = await (await page.$('.flow-canvas__minimap')).boundingBox();
  const viewBeforeMini = (await page.evaluate(probe)).view;
  const miniFrom = { x: miniBox.x + miniBox.width * 0.5, y: miniBox.y + miniBox.height * 0.5 };
  await page.mouse.move(miniFrom.x, miniFrom.y);
  await page.mouse.down();
  for (let step = 1; step <= 5; step += 1) {
    await page.mouse.move(miniFrom.x + step * 6, miniFrom.y + step * 4);
    await page.waitForTimeout(30);
  }
  await page.mouse.up();
  await page.waitForTimeout(350);
  const viewAfterMini = (await page.evaluate(probe)).view;
  check('小地图可拖、能平移视口', viewBeforeMini.x !== viewAfterMini.x || viewBeforeMini.y !== viewAfterMini.y, { before: viewBeforeMini, after: viewAfterMini });

  // ==================== ② 老画布（同一份图） ====================
  const openedOld = await open('native');
  const oldPage = openedOld.page;
  const nativeVisibleMs = openedOld.visibleMs;
  await oldPage.waitForTimeout(800);
  const old = await oldPage.evaluate(probe);
  const oldCanvas = await oldPage.$('.flow-canvas');
  await oldCanvas.screenshot({ path: path.join(SHOT_DIR, 'native.png') });
  await oldPage.click('.flow-canvas__zoom-btn[data-zoom="reset"]');
  await oldPage.waitForTimeout(400);
  const old100 = await oldPage.evaluate(probe);
  await oldCanvas.screenshot({ path: path.join(SHOT_DIR, 'native-100.png') });
  check('老画布仍是 native（切换器默认值没变）', old.engine === 'native', old.engine);

  // ---------- 视觉对比：同图同视口的逐节点像素差 ----------
  const source = await page.evaluate(probe); // 新画布（此刻已被交互改过，改用截图前的那一份）
  const vueShot = path.join(SHOT_DIR, 'vueflow.png');
  const oldShot = path.join(SHOT_DIR, 'native.png');
  const diffPage = await context.newPage();
  await diffPage.goto('about:blank');
  const diff = await diffPage.evaluate(
    async ([a, b, vueRects, nativeRects]) => {
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
      const diffIn = (x1, y1, x2, y2) => {
        let sum = 0;
        let big = 0;
        let count = 0;
        for (let y = Math.max(0, Math.floor(y1)); y < Math.min(A.height, Math.ceil(y2)); y += 1) {
          for (let x = Math.max(0, Math.floor(x1)); x < Math.min(A.width, Math.ceil(x2)); x += 1) {
            const i = (y * A.width + x) * 4;
            const delta = (Math.abs(A.data[i] - B.data[i]) + Math.abs(A.data[i + 1] - B.data[i + 1]) + Math.abs(A.data[i + 2] - B.data[i + 2])) / 3;
            sum += delta;
            if (delta > 40) big += 1;
            count += 1;
          }
        }
        return { mean: count ? sum / count : 0, bigRatio: count ? big / count : 0, count };
      };
      /**
       * 逐节点对比**不能**直接切同样的屏幕矩形再减：
       * 两套画布的 fitView 各自取整（0.506 vs 0.507），同一个节点在两图里缩放比差 0.2%，
       * 12px 文字的抗锯齿因此整体不同 —— 直接减会把「同一份 CSS」判成差异（实测 rawMean 22）。
       * 做法：把**各自**的卡片矩形裁出来，重采样到同一个 4× 尺寸再比 → 比的是「卡片长什么样」，
       * 不受整体缩放/取整影响。
       */
      const SCALE = 4;
      const cardDiff = async (ra, rb) => {
        const out = [];
        for (const [rectA, rectB] of ra.map((rect, index) => [rect, rb[index]])) {
          const w = 150 * SCALE;
          const h = 84 * SCALE;
          const canvasA = document.createElement('canvas');
          canvasA.width = w;
          canvasA.height = h;
          const ctxA = canvasA.getContext('2d');
          // 开平滑（高质量双线性）：把「0.2% 的缩放差」在重采样里抹平，比的是卡片长相
          ctxA.imageSmoothingEnabled = true;
          ctxA.imageSmoothingQuality = 'high';
          ctxA.drawImage(aImg, rectA.left, rectA.top, rectA.width, rectA.height, 0, 0, w, h);
          const canvasB = document.createElement('canvas');
          canvasB.width = w;
          canvasB.height = h;
          const ctxB = canvasB.getContext('2d');
          ctxB.imageSmoothingEnabled = true;
          ctxB.imageSmoothingQuality = 'high';
          ctxB.drawImage(bImg, rectB.left, rectB.top, rectB.width, rectB.height, 0, 0, w, h);
          const dataA = ctxA.getImageData(0, 0, w, h).data;
          const dataB = ctxB.getImageData(0, 0, w, h).data;
          let sum = 0;
          let big = 0;
          for (let i = 0; i < dataA.length; i += 4) {
            const delta = (Math.abs(dataA[i] - dataB[i]) + Math.abs(dataA[i + 1] - dataB[i + 1]) + Math.abs(dataA[i + 2] - dataB[i + 2])) / 3;
            sum += delta;
            if (delta > 40) big += 1;
          }
          const total = dataA.length / 4;
          out.push({ mean: sum / total, bigRatio: big / total, count: total });
        }
        return out;
      };
      const aImg = await load(a);
      const bImg = await load(b);
      const normalized = await cardDiff(vueRects, nativeRects);
      return {
        whole: diffIn(0, 0, A.width, A.height),
        nodes: vueRects.map((rect, index) => ({ key: rect.key, ...normalized[index] })),
        size: { a: [A.width, A.height], b: [B.width, B.height] },
      };
    },
    [
      'data:image/png;base64,' + fs.readFileSync(vueShot).toString('base64'),
      'data:image/png;base64,' + fs.readFileSync(oldShot).toString('base64'),
      vue.cards.map((card) => ({ key: card.key, left: card.rect.left - vue.wrapRect.left, top: card.rect.top - vue.wrapRect.top, width: card.rect.width, height: card.rect.height })),
      old.cards.map((card) => ({ key: card.key, left: card.rect.left - old.wrapRect.left, top: card.rect.top - old.wrapRect.top, width: card.rect.width, height: card.rect.height })),
    ],
  );
  const worstNode = diff.nodes.reduce((acc, item) => (item.mean > acc.mean ? item : acc), { key: null, mean: 0, bigRatio: 0 });
  check('两套画布截图尺寸一致', diff.size.a[0] === diff.size.b[0] && diff.size.a[1] === diff.size.b[1], diff.size);
  check('整图（fitView 视图）平均像素差 < 3/255', diff.whole.mean < 3, diff.whole);

  // ---------- 100% 缩放下的逐卡片 1:1 比对（决定性）----------
  // resetView 两边都是 {x:40,y:40,zoom:1}，所以同一个节点的屏幕矩形完全一致；
  // 此时不涉及重采样，像素差就是「渲染差异」本身（不是缩放取整的噪声）。
  const cardDiff100 = await diffPage.evaluate(
    async ([a, b, rects, bounds]) => {
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
      return rects
        .filter((rect) => rect.left >= 1 && rect.top >= 1 && rect.left + rect.width <= bounds.width - 1 && rect.top + rect.height <= bounds.height - 1)
        .map((rect) => {
          const x1 = Math.round(rect.left);
          const y1 = Math.round(rect.top);
          const x2 = Math.round(rect.left + rect.width);
          const y2 = Math.round(rect.top + rect.height);
          const diffAt = (dx, dy) => {
            let sum = 0;
            let big = 0;
            let count = 0;
            for (let y = y1; y < y2; y += 1) {
              for (let x = x1; x < x2; x += 1) {
                const sx = x + dx;
                const sy = y + dy;
                if (sx < 0 || sy < 0 || sx >= A.width || sy >= A.height) continue;
                const i = (y * A.width + x) * 4;
                const j = (sy * A.width + sx) * 4;
                const delta = (Math.abs(A.data[i] - B.data[j]) + Math.abs(A.data[i + 1] - B.data[j + 1]) + Math.abs(A.data[i + 2] - B.data[j + 2])) / 3;
                sum += delta;
                if (delta > 40) big += 1;
                count += 1;
              }
            }
            return { mean: count ? sum / count : 0, bigRatio: count ? big / count : 0, count };
          };
          // 100% 下两边缩放一致，只可能差一个整体平移（各自 zoomTo(1) 以视口中心为锚点，
          // 起点视口不同 → 平移量差几像素）。按 ±8px 搜最优整数对齐，报对齐后的残差。
          let best = { dx: 0, dy: 0, ...diffAt(0, 0) };
          for (let dy = -8; dy <= 8; dy += 1) {
            for (let dx = -8; dx <= 8; dx += 1) {
              if (dx === 0 && dy === 0) continue;
              const hit = diffAt(dx, dy);
              if (hit.mean < best.mean) best = { dx, dy, ...hit };
            }
          }
          return { key: rect.key, ...best, rect: [x1, y1, x2, y2] };
        });
    },
    [
      'data:image/png;base64,' + fs.readFileSync(path.join(SHOT_DIR, 'vueflow-100.png')).toString('base64'),
      'data:image/png;base64,' + fs.readFileSync(path.join(SHOT_DIR, 'native-100.png')).toString('base64'),
      vue100.cards.map((card) => ({ key: card.key, left: card.rect.left - vue100.wrapRect.left, top: card.rect.top - vue100.wrapRect.top, width: card.rect.width, height: card.rect.height })),
      { width: vue100.wrapRect.width, height: vue100.wrapRect.height },
    ],
  );
  // 100% 下两边缩放相同，节点**相对布局**必须完全一致：
  // 同一个节点在两图的屏幕坐标之差，应等于两图视口平移量之差（< 1px）。
  const shiftX = old100.view.x - vue100.view.x;
  const shiftY = old100.view.y - vue100.view.y;
  let maxSizeDelta = 0;
  let maxPosDelta = 0;
  let layoutSame = true;
  const missing = [];
  for (const card of vue100.cards) {
    const peer = old100.cards.find((item) => item.key === card.key);
    if (!peer) {
      layoutSame = false;
      missing.push(card.key);
      continue;
    }
    const sizeDelta = Math.max(Math.abs(peer.rect.width - card.rect.width), Math.abs(peer.rect.height - card.rect.height));
    const posDelta = Math.max(Math.abs(peer.rect.left - card.rect.left - shiftX), Math.abs(peer.rect.top - card.rect.top - shiftY));
    maxSizeDelta = Math.max(maxSizeDelta, sizeDelta);
    maxPosDelta = Math.max(maxPosDelta, posDelta);
    if (sizeDelta > 0.6 || posDelta > 1.5) {
      layoutSame = false;
    }
  }
  check('100% 缩放下节点尺寸一致、相对位置一致（扣除视口平移差后 < 1.5px）', layoutSame, {
    vue100: vue100.view,
    native100: old100.view,
    shiftX,
    shiftY,
    maxSizeDelta: Math.round(maxSizeDelta * 100) / 100,
    maxPosDelta: Math.round(maxPosDelta * 100) / 100,
    missing,
    counts: { vue: vue100.cards.length, native: old100.cards.length },
  });
  check('100% 缩放下可见卡片逐像素一致（平均差 < 3/255 且 >40 的像素 < 3%）', cardDiff100.length >= 4 && cardDiff100.every((item) => item.mean < 3 && item.bigRatio < 0.03), {
    compared: cardDiff100.length,
    // 残差来源：两边 zoomTo(1) 各自以视口中心为锚点，落点差 0.5–1px → 12px 文字的抗锯齿不同，
    // 不是样式差异（样式/尺寸/相对位置另有三条断言逐项钉住）。
    perCard: cardDiff100.map((item) => `${item.key}:${item.mean.toFixed(2)}@${item.dx},${item.dy}(>40:${(item.bigRatio * 100).toFixed(1)}%)`),
  });

  // ==================== ③ 只读画布（readonly=true，已发布版本）====================
  // 调用方一行没改：已发布版本在编辑器里就是只读打开，`readonly` 契约必须等价
  // （不画句柄、不能拖、不能拉线、不显示自动布局；缩放工具栏仍在）。
  if (READONLY_DEF_ID) {
    const roPage = await context.newPage();
    roPage.on('console', (message) => {
      if (message.type() === 'error') consoleErrors.other.push(message.text());
    });
    roPage.on('pageerror', (error) => pageErrors.other.push(error.message));
    await roPage.goto(`${BASE}/flow/def/${READONLY_DEF_ID}/edit?canvas=vueflow`, { waitUntil: 'domcontentloaded' });
    await roPage.waitForSelector('.flow-node__card', { timeout: 30000 });
    await roPage.waitForTimeout(1500);
    const ro = await roPage.evaluate(() => ({
      engine: Boolean(document.querySelector('.vue-flow')),
      cards: document.querySelectorAll('.flow-node__card').length,
      handles: document.querySelectorAll('.vue-flow__handle').length,
      // VUE-FLOW-2 起：只读态**保留** Handle（Vue Flow 靠它算锚点，摘掉会让连线变成底部出/顶部进），
      // 但视觉隐藏（opacity: 0）+ 不可拖。所以这里断言的是「在 DOM 里、但看不见」。
      handleOpacity: [...new Set([...document.querySelectorAll('.vue-flow__handle')].map((el) => getComputedStyle(el).opacity))],
      handlePositions: [...new Set([...document.querySelectorAll('.vue-flow__handle')].map((el) => el.getAttribute('data-handlepos')))].sort(),
      layoutActions: document.querySelectorAll('.flow-canvas__action').length,
      zoomButtons: document.querySelectorAll('.flow-canvas__zoom-btn').length,
      readonly: Boolean(document.querySelector('.flow-canvas.is-readonly')),
      hint: document.querySelector('.flow-canvas__hint')?.textContent?.replace(/\s+/g, ' ').trim() ?? null,
    }));
    check('只读画布也切到 Vue Flow（已发布版本）', ro.engine && ro.cards === 3, ro);
    check(
      '只读：Handle 保留在 DOM（2×节点数）但 opacity:0 不可见、方向 left/right、不显示自动布局按钮',
      ro.handles === ro.cards * 2 && ro.handleOpacity.join() === '0' && ro.handlePositions.join() === 'left,right' && ro.layoutActions === 0 && ro.zoomButtons === 4 && ro.readonly,
      ro,
    );
    check('只读：底部提示改成只读说明（不再提示「拖出连线」）', Boolean(ro.hint) && ro.hint.includes('只读模式') && !ro.hint.includes('拖出连线'), ro.hint);
    await roPage.screenshot({ path: path.join(SHOT_DIR, 'readonly-published.png') });
  }

  // ---------- 控制台 ----------
  // 判定标准：新画布**不引入新报错**（页面本来就有的一批 401 之类，两套画布都会出现）
  const nativeErrSet = new Set([...consoleErrors.native, ...pageErrors.native]);
  const vueErrAll = [...consoleErrors.vueflow, ...pageErrors.vueflow];
  const newErrors = vueErrAll.filter((message) => !nativeErrSet.has(message));
  check('新画布控制台 0 新增报错（与老画布同集合）', newErrors.length === 0, {
    newErrors,
    vueflow: vueErrAll,
    native: [...nativeErrSet],
    failedResponses,
  });

  const report = {
    generatedAt: new Date().toISOString(),
    baseUrl: BASE,
    defId: DEF_ID,
    browser: BROWSER,
    canvasVisibleMs,
    nativeVisibleMs,
    checks,
    passed: checks.filter((item) => item.ok).length,
    failed: checks.filter((item) => !item.ok).length,
    geometry: { vueflow: vue, native: old },
    pixelDiff: { fitView: diff, at100: cardDiff100, worstNormalizedCard: worstNode },
    consoleErrors,
    pageErrors,
    failedResponses,
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
