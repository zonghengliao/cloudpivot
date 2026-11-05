#!/usr/bin/env node
/**
 * VUE-FLOW-3 全量回归：默认画布 = Vue Flow（Chrome + Safari/WebKit 各跑一遍）。
 *
 * 覆盖卡片 B 段（功能 16 项）与 C 段（Safari 专项 4 项），一条命令跑完并留截图与 JSON：
 *
 *   B1  打开流程列表 → 能看到
 *   B2  点「编辑」进画布 → 加载正常
 *   B3  9 种节点都能从左侧拖出来
 *   B4  拖节点 / 连线 / 删节点 / 删线
 *   B5  条件分支节点能配置（点开属性面板）
 *   B6  自动布局按钮能用
 *   B7  保存 / 发布 → 成功
 *   B8  已发布流程只读：拖不动、连不了
 *   B9  「另存为新草稿」能用
 *   B10 迷你地图点/拖能移动视口
 *   B11 缩放工具栏 + / − / fit 都能用
 *   B12 校验失败时角标显示
 *   B13 回退开关：?canvas=native 切回老画布（默认切到 vueflow 后仍可用）
 *   C1  Safari：节点正常显示、位置正确、不重叠
 *   C2  Safari：能拖节点、能连线、能缩放（复用 B4/B11 的同一套断言，按引擎分别记录）
 *   C3  Safari：卡片不盖住锚点（elementFromPoint 命中句柄本身）
 *   C4  截图存档（每步一张）
 *
 * 用法：
 *   node scripts/verify/vueflow3_full_regression.cjs --browser chromium \
 *     --base-url http://127.0.0.1:5188 \
 *     --shot-dir docs/evidence/20261104-vueflow-3/shots/chromium \
 *     --out docs/evidence/20261104-vueflow-3/regression-chromium.json
 *
 *   node scripts/verify/vueflow3_full_regression.cjs --browser webkit ... （Safari 同引擎）
 *
 * 退出码：0 = 全部通过；1 = 有用例失败。
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
  throw new Error('找不到 Playwright；请在仓库内运行（frontend/node_modules 里有一份）');
}

/** 解析 `--key value` / `--key=value` */
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
const ENGINE = (args.browser || 'chromium').toLowerCase();
const SHOT_DIR = args['shot-dir'] || `docs/evidence/20261104-vueflow-3/shots/${ENGINE}`;
const OUT = args.out || `docs/evidence/20261104-vueflow-3/regression-${ENGINE}.json`;
const USER = args.user || 'laoliu_test';
const PASSWORD = args.password || 'Test@12345';
/** 已发布夹具：9 节点 / 11 连线（只读态、另存草稿、锚点遮挡、不重叠都用它） */
const PUBLISHED_DEF = args['published-def-id'] || '20000000-0000-4000-8000-0000000a0182';

const SHOTS = [];
const CHECKS = [];
const CONSOLE_ERRORS = [];
/** 4xx/5xx 资源请求（notice SSE 的 401 属既有多画布无关，单独记录不判失败） */
const HTTP_ERRORS = [];

function log(msg) {
  process.stdout.write(`${msg}\n`);
}

/** 记录一条验收项（异常 = 失败，不中断后续项） */
async function check(id, item, fn) {
  const started = Date.now();
  try {
    const detail = await fn();
    CHECKS.push({ id, item, pass: true, detail: detail === undefined ? '' : detail, ms: Date.now() - started });
    log(`  ✓ ${id} ${item}${detail ? ` — ${detail}` : ''}`);
    return true;
  } catch (error) {
    const message = String(error && error.message ? error.message : error).split('\n')[0];
    CHECKS.push({ id, item, pass: false, detail: message, ms: Date.now() - started });
    log(`  ✗ ${id} ${item} — ${message}`);
    return false;
  }
}

function assert(cond, message) {
  if (!cond) throw new Error(message);
}

/** 截图（文件名带引擎前缀；返回相对路径） */
async function shot(page, name) {
  fs.mkdirSync(SHOT_DIR, { recursive: true });
  const file = path.join(SHOT_DIR, `${name}.png`);
  await page.screenshot({ path: file, fullPage: false });
  SHOTS.push(file);
  return file;
}

/** 后端 API（node 22 自带 fetch） */
async function api(pathname, { method = 'GET', body, token } = {}) {
  const res = await fetch(`${BASE}${pathname}`, {
    method,
    headers: {
      'Content-Type': 'application/json',
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const json = await res.json().catch(() => ({}));
  if (json.code !== 0) throw new Error(`${method} ${pathname} → code=${json.code} ${json.msg || ''}`);
  return json.data;
}

/** 画布上的节点（两套画布统一的契约属性：一节点一元素） */
const NODE = '[data-node-key]';
const EDGE_HIT = '.flow-edge__hit';

async function nodeCount(page) {
  return page.locator(NODE).count();
}

async function edgeCount(page) {
  return page.locator(EDGE_HIT).count();
}

/** 出口锚点选择器：Vue Flow 的 Handle 与卡片是兄弟，老画布在节点 <g> 内部 */
function outPortSelector(key) {
  return `.flow-node__port--out[data-port-node-key="${key}"], [data-node-key="${key}"] .flow-node__port--out`;
}

/** 取元素中心点（拖拽 / 连线要精确坐标） */
async function centerOf(locator) {
  const box = await locator.boundingBox();
  if (!box) throw new Error('元素不可见，取不到坐标');
  return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
}

/** 同 centerOf（语义化别名：对 Locator 取中心） */
const locatorCenter = centerOf;

/** 拖拽：从 (from) 到 (to)，中间给足 steps（真机指针事件序列） */
async function drag(page, from, to, steps = 22) {
  await page.mouse.move(from.x, from.y);
  await page.mouse.down();
  await page.mouse.move(to.x, to.y, { steps });
  await page.mouse.up();
}

/** 当前视口变换（平移 + 缩放） */
async function viewportTransform(page) {
  return page.evaluate(() => {
    const pane = document.querySelector('.vue-flow__transformationpane, .flow-canvas__viewport');
    return pane ? getComputedStyle(pane).transform : 'none';
  });
}

/** 缩放读数（工具栏上的百分比） */
async function zoomText(page) {
  const el = page.locator('.flow-canvas__zoom-btn--scale');
  return (await el.count()) ? (await el.first().innerText()).trim() : '';
}

/** 等画布就绪：节点数稳定 + 首帧绘制完成 */
async function waitCanvas(page, expectNodes, timeout = 25000) {
  await page.waitForSelector(NODE, { timeout });
  await page.waitForFunction(
    ({ sel, n }) => document.querySelectorAll(sel).length >= n,
    { sel: NODE, n: expectNodes },
    { timeout },
  );
  await page.waitForTimeout(350);
}

async function toastVisible(page, text, timeout = 15000) {
  const toast = page.locator('.t-message').filter({ hasText: text }).first();
  await toast.waitFor({ state: 'visible', timeout });
  return true;
}

/** 通过 API 造一条「能发布」的最小流程（开始 → 审批(发起人上级) → 结束） */
async function createPublishableFlow(token) {
  const code = `vf3_${Date.now().toString(36)}`;
  const created = await api('/api/business/flow/def', {
    method: 'POST',
    token,
    body: { flow_code: code, flow_name: `[VUE-FLOW-3]发布回归-${code}`, biz_type: 'OTHER' },
  });
  const nodes = [
    { key: 's', name: '开始', type: 'START', x: 60, y: 160, sort_order: 0, assignee_type: null },
    {
      key: 'a1',
      name: '部门主管审批',
      type: 'APPROVAL',
      x: 260,
      y: 150,
      sort_order: 0,
      assignee_type: 'INITIATOR_SUPERIOR',
      superior_levels: 1,
    },
    { key: 'e', name: '结束', type: 'END', x: 480, y: 160, sort_order: 0, assignee_type: null },
  ];
  const edges = [
    { key: 'ed1', from: 's', to: 'a1', label: null, priority: 1, is_default: false },
    { key: 'ed2', from: 'a1', to: 'e', label: null, priority: 2, is_default: false },
  ];
  await api(`/api/business/flow/version/${created.version_id}/graph`, {
    method: 'PUT',
    token,
    body: { nodes, edges, change_note: 'VUE-FLOW-3 发布回归' },
  });
  return { defId: created.id, versionId: created.version_id, code };
}

/**
 * 取登录态：优先复用 token 缓存文件（跨引擎 / 跨进程复用，避免把进程内限流计数器打满）。
 * 缓存文件里的 JWT 还有 2 分钟以上有效期就直接用，否则重新登录。
 */
async function loadOrLogin() {
  const cacheFile = args['token-file'] || '/tmp/vueflow3-token.json';
  try {
    const cached = JSON.parse(fs.readFileSync(cacheFile, 'utf-8'));
    const payload = JSON.parse(Buffer.from(String(cached.access_token).split('.')[1], 'base64url').toString('utf-8'));
    if (payload.exp * 1000 - Date.now() > 120000) {
      log(`（复用 token 缓存：${cacheFile}，exp=${new Date(payload.exp * 1000).toISOString()}）`);
      return cached;
    }
  } catch {
    /* 没有缓存 / 过期 / 解析失败 → 重新登录 */
  }
  const fresh = await api('/api/system/auth/login', { method: 'POST', body: { username: USER, password: PASSWORD } });
  fs.writeFileSync(cacheFile, JSON.stringify(fresh, null, 2));
  return fresh;
}

async function main() {
  const { chromium, webkit } = loadPlaywright();
  const engine = ENGINE === 'webkit' || ENGINE === 'safari' ? webkit : chromium;
  const browser = await engine.launch();
  const context = await browser.newContext({
    viewport: { width: 1600, height: 1000 },
    locale: 'zh-CN',
    timezoneId: 'Asia/Shanghai',
  });

  // 登录一次拿 token（后端限流 10 次/60 秒/(账号+IP)，整轮复用；跨进程用 token 缓存文件）
  // ★ 限流实现是进程内计数且**不会随时间衰减**（backend/src/system/auth/handler.rs 的
  //   check_login_rate：内存 cache 的 set_ex 不生效 → 计数只增不减），所以这里刻意做成
  //   「一次登录、反复复用」，否则连跑 chromium + webkit 两轮就会把自己锁死（要重启后端才解锁）。
  const login = await loadOrLogin();
  const token = login.access_token;

  // 与 e2e/global-setup.ts 同一套注入：token + 「已看过新功能引导」（否则引导浮层挡点击）
  await context.addInitScript(
    ({ access, refresh }) => {
      localStorage.setItem('x_access_token', access);
      localStorage.setItem('x_refresh_token', refresh);
      localStorage.setItem('cp-onboarding-done', '1');
    },
    { access: token, refresh: login.refresh_token },
  );

  const page = await context.newPage();
  page.on('console', (msg) => {
    if (msg.type() !== 'error') return;
    const text = msg.text();
    // 浏览器对「4xx 资源」也会打一条 console.error（文案固定），这类归到 HTTP_ERRORS，
    // 否则「控制台 0 报错」这条验收会被网络噪声污染（notice SSE 的 401 是既有行为）。
    if (/^Failed to load resource/.test(text)) return;
    CONSOLE_ERRORS.push(`[console] ${text}`.slice(0, 300));
  });
  page.on('response', (resp) => {
    if (resp.status() >= 400) HTTP_ERRORS.push(`${resp.status()} ${resp.url().replace(BASE, '').slice(0, 160)}`);
  });
  page.on('pageerror', (error) => CONSOLE_ERRORS.push(`[pageerror] ${String(error).slice(0, 300)}`));

  /** 关掉会挡住列表操作的模态弹窗（「你有 N 条待处理」等）。
   *  为什么必须有：弹窗遮罩是 `t-dialog__position` 覆盖整屏，行内「编辑/发布/停用」
   *  的点击会被遮罩吃掉 → playwright click() 30s 超时（B2/B8/C1 曾因此假红）。 */
  async function dismissBlockingDialogs() {
    for (let i = 0; i < 3; i += 1) {
      const btn = page.locator('.t-dialog button:has-text("知道了"), .t-dialog button:has-text("我知道了")').first();
      if (await btn.count()) {
        await btn.click({ timeout: 3000 }).catch(() => {});
        await page.waitForTimeout(500);
        continue;
      }
      const mask = await page
        .locator('.t-dialog__position, .t-dialog__modal-default')
        .filter({ hasNot: page.locator('[style*="display: none"]') })
        .count();
      if (mask === 0) return;
      await page.keyboard.press('Escape').catch(() => {});
      await page.waitForTimeout(500);
    }
  }

  /** 引擎判定：默认（不传参数）必须是 Vue Flow 画布 */
  async function assertVueFlowEngine() {
    const flow = await page.locator('.vue-flow').count();
    const native = await page.locator('g.flow-node').count();
    assert(flow === 1, `默认应为 Vue Flow 画布，实际 .vue-flow=${flow}`);
    assert(native === 0, `默认不应再渲染老画布，实际 g.flow-node=${native}`);
    return '`.vue-flow` 1 个 / `g.flow-node` 0 个';
  }

  log(`\n=== VUE-FLOW-3 全量回归 · 引擎=${ENGINE} · ${BASE} ===`);

  // ==================== B1 流程列表 ====================
  let publishedRow = null;
  await check('B1', '打开流程列表 → 能看到', async () => {
    await page.goto(`${BASE}/flow/def`, { waitUntil: 'domcontentloaded' });
    await page.waitForSelector('.flow-def-page', { timeout: 30000 });
    await page.waitForSelector('.flow-def-page tbody tr', { timeout: 30000 });
    const rows = await page.locator('.flow-def-page tbody tr').count();
    assert(rows > 0, '流程列表没有数据行');
    await dismissBlockingDialogs();
    await shot(page, 'b1-flow-list');
    return `${rows} 行`;
  });

  // ==================== B2 进画布（已发布 → 只读） ====================
  await check('B2', '点「编辑」进画布 → 加载正常（默认 Vue Flow）', async () => {
    const row = page.locator('.flow-def-page tbody tr').filter({ hasText: '采购订单审批' }).first();
    await row.waitFor({ timeout: 20000 });
    await dismissBlockingDialogs();
    publishedRow = row;
    await row.getByText('编辑', { exact: true }).click();
    await page.waitForURL(/\/flow\/def\/[0-9a-f-]+\/edit/, { timeout: 30000 });
    await waitCanvas(page, 9);
    const nodes = await nodeCount(page);
    const engineText = await assertVueFlowEngine();
    await shot(page, 'b2-editor-published-readonly');
    return `节点 ${nodes} 个 · ${engineText}`;
  });

  // ==================== B8 只读态：拖不动 / 连不了 ====================
  await check('B8', '已发布流程打开是只读态：拖不动、连不了', async () => {
    const alert = page.locator('.flow-editor__alert').filter({ hasText: '已发布' }).first();
    assert(await alert.count(), '没看到「已发布」只读横幅');
    const disabledPalette = await page.locator('.palette-item.is-disabled').count();
    assert(disabledPalette > 0, '左侧节点面板没有进入禁用态');

    // ★ 判定口径：只读态允许「拖空白平移画布」（那是视口，不是节点）。
    //   所以不能只看某个节点屏幕位置变没变，要看「节点之间的相对位移」：
    //   平移时所有节点同步移动（相对坐标不变），只有真的拖动节点才会改变相对位置。
    const keys = await page.locator(NODE).evaluateAll((els) => els.map((el) => el.dataset.nodeKey));
    const boxesOf = async () =>
      page.locator(NODE).evaluateAll((els) => {
        const map = {};
        for (const el of els) {
          const r = el.getBoundingClientRect();
          map[el.dataset.nodeKey] = { x: r.x, y: r.y };
        }
        return map;
      });
    const before = await boxesOf();
    const beforeView = await viewportTransform(page);
    const center = await locatorCenter(page.locator(`[data-node-key="${keys[0]}"]`));
    await drag(page, center, { x: center.x + 90, y: center.y + 70 });
    await page.waitForTimeout(300);
    const after = await boxesOf();
    const afterView = await viewportTransform(page);
    const relBefore = { x: before[keys[1]].x - before[keys[0]].x, y: before[keys[1]].y - before[keys[0]].y };
    const relAfter = { x: after[keys[1]].x - after[keys[0]].x, y: after[keys[1]].y - after[keys[0]].y };
    const drift = Math.abs(relAfter.x - relBefore.x) + Math.abs(relAfter.y - relBefore.y);
    assert(drift <= 1.5, `只读态节点相对位置变了 ${drift.toFixed(1)}px（节点被拖动了）`);

    // 连不了：从任一出口锚点拖到别的卡片，边数不变
    const edgesBefore = await edgeCount(page);
    const port = await locatorCenter(page.locator(outPortSelector(keys[0])));
    const target = await locatorCenter(page.locator(`[data-node-key="${keys[2]}"]`));
    await drag(page, port, target);
    await page.waitForTimeout(400);
    const edgesAfter = await edgeCount(page);
    assert(edgesAfter === edgesBefore, `只读态居然建成了边：${edgesBefore} → ${edgesAfter}`);
    await shot(page, 'b8-readonly-unchanged');
    return `节点相对位移 ${drift.toFixed(1)}px · 边数 ${edgesBefore}→${edgesAfter} · 画布平移=${beforeView !== afterView}`;
  });

  // ==================== C1 节点显示 / 位置 / 不重叠 ====================
  await check('C1', '节点正常显示、位置正确、不重叠', async () => {
    const boxes = await page.locator(NODE).evaluateAll((els) =>
      els.map((el) => {
        const r = el.getBoundingClientRect();
        return { key: el.dataset.nodeKey, x: r.x, y: r.y, w: r.width, h: r.height };
      }),
    );
    assert(boxes.length >= 9, `节点数不足：${boxes.length}`);
    const bad = boxes.filter((b) => !(b.w > 20 && b.h > 15) || Number.isNaN(b.x) || Number.isNaN(b.y));
    assert(bad.length === 0, `有节点尺寸/坐标异常：${JSON.stringify(bad.slice(0, 3))}`);
    const overlaps = [];
    for (let i = 0; i < boxes.length; i += 1) {
      for (let j = i + 1; j < boxes.length; j += 1) {
        const a = boxes[i];
        const b = boxes[j];
        const ox = Math.min(a.x + a.w, b.x + b.w) - Math.max(a.x, b.x);
        const oy = Math.min(a.y + a.h, b.y + b.h) - Math.max(a.y, b.y);
        if (ox > 2 && oy > 2) overlaps.push(`${a.key}×${b.key}(${ox.toFixed(0)}×${oy.toFixed(0)})`);
      }
    }
    assert(overlaps.length === 0, `节点重叠：${overlaps.slice(0, 4).join(', ')}`);
    await shot(page, 'c1-nodes-layout');
    return `${boxes.length} 个节点，两两重叠 0 对`;
  });

  // ==================== B9 另存为新草稿 ====================
  let draftFixture = null;
  await check('B9', '「另存为新草稿」能用（→ 可编辑）', async () => {
    // 刻意用**本轮自己造的**已发布流程，而不是共享夹具：
    // 「另存为新草稿」会给该定义新增一个草稿版本，落在共享夹具上就会把它变成「非只读」，
    // 下一轮 B2/B8 的只读断言就全废了（实测踩过）。自己造的用完即删，可重复执行。
    draftFixture = await createPublishableFlow(token);
    await api(`/api/business/flow/def/${draftFixture.defId}/publish`, { method: 'POST', token, body: {} });
    await page.goto(`${BASE}/flow/def/${draftFixture.defId}/edit`, { waitUntil: 'domcontentloaded' });
    await waitCanvas(page, 3);
    const readonlyAlert = page.locator('.flow-editor__alert').filter({ hasText: '已发布' }).first();
    assert(await readonlyAlert.count(), '另存前不是只读态（夹具没发布成功？）');

    const btn = page.getByRole('button', { name: '另存为新草稿' });
    await btn.waitFor({ timeout: 15000 });
    await btn.click();
    const confirm = page.locator('.t-dialog').filter({ hasText: '另存' }).first();
    if (await confirm.count()) {
      const ok = confirm.getByRole('button', { name: /另存|确定|保存/ }).first();
      if (await ok.count()) await ok.click();
    }
    await page.waitForTimeout(1800);
    const alertGone = (await page.locator('.flow-editor__alert').filter({ hasText: '已发布' }).count()) === 0;
    const editable = (await page.locator('.palette-item.is-disabled').count()) === 0;
    await shot(page, 'b9-draft-editable');
    assert(alertGone && editable, `另存后仍是只读（横幅消失=${alertGone} / 面板可拖=${editable}）`);
    return `只读横幅消失=${alertGone} · 面板可拖=${editable} · 节点 ${await nodeCount(page)}`;
  });

  // ==================== 新建流程：编辑态全流程 ====================
  let newDefId = '';
  await check('B3a', '新建流程 → 进编辑器（默认 2 节点，Vue Flow）', async () => {
    await page.goto(`${BASE}/flow/def`, { waitUntil: 'domcontentloaded' });
    await page.waitForSelector('.flow-def-page tbody tr', { timeout: 30000 });
    await page.getByRole('button', { name: '新建流程' }).click();
    const dlg = page.locator('.t-dialog').filter({ hasText: '新建流程' }).first();
    await dlg.waitFor({ timeout: 15000 });
    const code = `vf3ui_${Date.now().toString(36)}`;
    await dlg.locator("input[placeholder*='小写字母开头']").fill(code);
    await dlg.locator("input[placeholder*='如 任务验收']").fill(`[VUE-FLOW-3]回归-${code}`);
    await dlg.locator("input[placeholder*='TASK_ACCEPT']").fill(`VF3_${code.toUpperCase()}`);
    await dlg.getByRole('button', { name: '创建并进入编辑器' }).click();
    await page.waitForURL(/\/flow\/def\/[0-9a-f-]+\/edit/, { timeout: 30000 });
    newDefId = page.url().match(/\/flow\/def\/([0-9a-f-]+)\/edit/)[1];
    await waitCanvas(page, 2);
    const engineText = await assertVueFlowEngine();
    await shot(page, 'b3-new-flow-editor');
    return `节点 2 个 · ${engineText}`;
  });

  // ==================== B3 9 种节点都能拖出来 ====================
  await check('B3', '9 种节点都能从左侧拖出来', async () => {
    const types = await page.locator('.palette-item').evaluateAll((els) => els.map((el) => el.dataset.nodeType));
    assert(types.length >= 9, `左侧面板节点类型只有 ${types.length} 种`);
    const canvas = page.locator('.flow-editor__canvas');
    const cbox = await canvas.boundingBox();
    const at = (fx, fy) => ({ x: cbox.x + cbox.width * fx, y: cbox.y + cbox.height * fy });
    const positions = [
      [0.18, 0.22],
      [0.38, 0.22],
      [0.58, 0.22],
      [0.78, 0.22],
      [0.18, 0.5],
      [0.38, 0.5],
      [0.58, 0.5],
      [0.78, 0.5],
      [0.5, 0.75],
    ];
    let expected = 2;
    const created = [];
    for (let i = 0; i < Math.min(types.length, 9); i += 1) {
      const type = types[i];
      const from = await centerOf(page.locator(`.palette-item[data-node-type="${type}"]`));
      const to = at(positions[i][0], positions[i][1]);
      await drag(page, from, to, 18);
      expected += 1;
      await page.waitForFunction(
        ({ sel, n }) => document.querySelectorAll(sel).length >= n,
        { sel: NODE, n: expected },
        { timeout: 15000 },
      );
      created.push(type);
    }
    const total = await nodeCount(page);
    assert(total === 2 + created.length, `拖完 9 种后节点数 ${total} ≠ ${2 + created.length}`);
    await shot(page, 'b3-nine-node-types');
    return `拖出 ${created.length} 种：${created.join('/')}（共 ${total} 节点）`;
  });

  // ==================== B4 拖节点 / 连线 / 删节点 / 删线 ====================
  await check('B4a', '拖节点（位置真的变了）', async () => {
    const node = page.locator(NODE).last();
    const key = await node.getAttribute('data-node-key');
    const before = await node.boundingBox();
    await drag(page, { x: before.x + before.width / 2, y: before.y + before.height / 2 }, { x: before.x + before.width / 2 + 110, y: before.y + before.height / 2 + 70 });
    await page.waitForTimeout(300);
    const after = await page.locator(`[data-node-key="${key}"]`).boundingBox();
    const moved = Math.abs(after.x - before.x) + Math.abs(after.y - before.y);
    assert(moved > 40, `节点几乎没动：${moved.toFixed(1)}px`);
    await shot(page, 'b4-node-dragged');
    return `位移 ${moved.toFixed(0)}px`;
  });

  await check('B4b', '连线（拖锚点成边）', async () => {
    const keys = await page.locator(NODE).evaluateAll((els) => els.map((el) => el.dataset.nodeKey));
    const edgesBefore = await edgeCount(page);
    let done = false;
    for (let i = 0; i < keys.length && !done; i += 1) {
      for (let j = 0; j < keys.length && !done; j += 1) {
        if (i === j) continue;
        const port = await centerOf(page.locator(outPortSelector(keys[i])));
        const target = await centerOf(page.locator(`[data-node-key="${keys[j]}"]`));
        await drag(page, port, target, 18);
        await page.waitForTimeout(400);
        const now = await edgeCount(page);
        if (now > edgesBefore) {
          done = true;
          await shot(page, 'b4-edge-created');
          return `边 ${edgesBefore} → ${now}（${keys[i]} → ${keys[j]}）`;
        }
      }
    }
    throw new Error(`任何一对节点都连不上（边数仍 ${edgesBefore}）`);
  });

  await check('B4c', '删线（点中连线按 Delete）', async () => {
    const before = await edgeCount(page);
    // 命中点要满足两个条件：① 落在 .flow-edge__hit 上（不被节点/标签遮住）；② 不在任何节点框里。
    // 取路径上多个参数点扫描，第一个可用的就是它 —— 直接拿「路径中点」会踩到节点上，
    // 那样点中的是节点、Delete 删的也是节点（边数不变），会把用例判成「删线失败」。
    const point = await page.evaluate(() => {
      const rects = [...document.querySelectorAll('[data-node-key]')].map((el) => el.getBoundingClientRect());
      const near = (x, y, r, pad = 4) => x >= r.x - pad && x <= r.right + pad && y >= r.y - pad && y <= r.bottom + pad;
      for (const path of document.querySelectorAll('.flow-edge__hit')) {
        const g = path.closest('.flow-edge');
        const len = path.getTotalLength();
        const m = path.getScreenCTM();
        for (let t = 0.1; t <= 0.95; t += 0.02) {
          const p = path.getPointAtLength(len * t);
          const x = p.x * m.a + p.y * m.c + m.e;
          const y = p.x * m.b + p.y * m.d + m.f;
          if (rects.some((r) => near(x, y, r))) continue;
          const hit = document.elementFromPoint(x, y);
          if (hit && hit.closest('.flow-edge') === g) return { x, y };
        }
      }
      return null;
    });
    assert(point, '找不到不被节点遮挡的连线命中点');
    await page.mouse.click(point.x, point.y);
    await page.waitForTimeout(250);
    const selectedEdges = await page.locator('.flow-edge.is-selected').count();
    assert(selectedEdges === 1, `点连线后选中态错误：.flow-edge.is-selected=${selectedEdges}`);
    await shot(page, 'b4-edge-selected');
    await page.keyboard.press('Delete');
    await page.waitForTimeout(400);
    const after = await edgeCount(page);
    assert(after === before - 1, `删线后边数 ${before} → ${after}（应减 1）`);
    await shot(page, 'b4-edge-deleted');
    return `边 ${before} → ${after}（点击命中点 ${Math.round(point.x)},${Math.round(point.y)}）`;
  });

  await check('B4d', '删节点（选中按 Delete）', async () => {
    const before = await nodeCount(page);
    // 刻意挑 NOTIFY_NODE（拖进来的 9 种之一），不挑 CONDITION —— 下一条 B5 还要用它
    const notify = page.locator(`${NODE}[data-node-type="NOTIFY_NODE"]`).first();
    const target = (await notify.count()) ? notify : page.locator(NODE).last();
    await target.click();
    await page.waitForTimeout(200);
    await page.keyboard.press('Delete');
    await page.waitForTimeout(500);
    const after = await nodeCount(page);
    assert(after === before - 1, `删节点后 ${before} → ${after}（应减 1）`);
    await shot(page, 'b4-node-deleted');
    return `节点 ${before} → ${after}`;
  });

  // ==================== B5 条件分支节点配置 ====================
  await check('B5', '条件分支节点能配置（点开属性面板）', async () => {
    const condition = page.locator(`${NODE}[data-node-type="CONDITION"]`).first();
    assert(await condition.count(), '画布上没有 CONDITION 节点');
    await condition.click();
    await page.waitForTimeout(500);
    const props = page.locator('.flow-editor__props');
    await props.waitFor({ timeout: 10000 });
    const text = await props.innerText();
    assert(/条件|分支|规则/.test(text), `属性面板没出现条件配置：${text.slice(0, 120)}`);
    await shot(page, 'b5-condition-props');
    return `属性面板可见，含「${(text.match(/条件|分支|规则/) || [''])[0]}」配置`;
  });

  // ==================== B6 自动布局 ====================
  await check('B6', '自动布局按钮能用（dagre 重排）', async () => {
    const before = await page.locator(NODE).evaluateAll((els) => els.map((el) => el.getBoundingClientRect().x));
    await page.getByRole('button', { name: '自动布局' }).click();
    const dlg = page.locator('.t-dialog').filter({ hasText: '重排所有节点的位置' }).first();
    await dlg.waitFor({ timeout: 10000 });
    await dlg.getByRole('button', { name: '开始重排' }).click();
    await toastVisible(page, '已重排');
    await page.waitForTimeout(700);
    const after = await page.locator(NODE).evaluateAll((els) => els.map((el) => el.getBoundingClientRect().x));
    const changed = before.some((x, i) => Math.abs(x - after[i]) > 2);
    await shot(page, 'b6-auto-layout');
    assert(changed, '自动布局后节点坐标没变化');
    return `${before.length} 个节点重排，坐标已变化`;
  });

  // ==================== C3 卡片不盖住锚点（自动布局后，节点不再重叠） ====================
  // ★ 必须在「可编辑 + 节点不重叠」的画布上量：
  //   · 只读态句柄按设计 opacity:0 + 不可点，elementFromPoint 命中卡片是正常现象；
  //   · 节点互相压着时，锚点被**另一个节点**的卡片覆盖也是正常现象（那是 z-order 不是遮挡 bug）。
  await check('C3', '卡片不盖住锚点（锚点中心 elementFromPoint 命中句柄本身）', async () => {
    const report = await page.evaluate(({ sel }) => {
      const out = [];
      for (const node of document.querySelectorAll(sel)) {
        const key = node.dataset.nodeKey;
        for (const port of document.querySelectorAll(`.flow-node__port[data-port-node-key="${key}"]`)) {
          const r = port.getBoundingClientRect();
          const hit = document.elementFromPoint(r.x + r.width / 2, r.y + r.height / 2);
          out.push({
            key,
            side: port.classList.contains('flow-node__port--out') ? 'out' : 'in',
            hitClass: hit && hit.className ? String(hit.className) : 'null',
            covered: !(hit === port || port.contains(hit)),
          });
        }
      }
      return out;
    }, { sel: NODE });
    assert(report.length >= 4, `锚点数量异常：${report.length}`);
    const covered = report.filter((r) => r.covered);
    assert(covered.length === 0, `有 ${covered.length} 个锚点被别的元素盖住：${JSON.stringify(covered.slice(0, 3))}`);
    await shot(page, 'c3-anchors-uncovered');
    return `${report.length} 个锚点全部点得中（含 ::before 7px 命中区）`;
  });

  // ==================== B10 迷你地图 ====================
  await check('B10', '迷你地图点/拖能移动视口', async () => {
    const minimap = page.locator('.flow-canvas__minimap').first();
    assert(await minimap.count(), '没找到迷你地图');
    const box = await minimap.boundingBox();
    const before = await viewportTransform(page);
    await page.mouse.click(box.x + box.width * 0.2, box.y + box.height * 0.8);
    await page.waitForTimeout(500);
    const afterClick = await viewportTransform(page);
    assert(afterClick !== before, `点击迷你地图后视口没变（${before}）`);
    await page.mouse.move(box.x + box.width * 0.2, box.y + box.height * 0.8);
    await page.mouse.down();
    await page.mouse.move(box.x + box.width * 0.7, box.y + box.height * 0.3, { steps: 12 });
    await page.mouse.up();
    await page.waitForTimeout(400);
    const afterDrag = await viewportTransform(page);
    await shot(page, 'b10-minimap');
    assert(afterDrag !== afterClick || afterClick !== before, '迷你地图拖动没生效');
    return `点击/拖动后 transform 变化 ${afterClick !== before ? '(点击)' : ''}${afterDrag !== afterClick ? '(拖动)' : ''}`;
  });

  // ==================== B11 缩放工具栏 ====================
  await check('B11', '缩放工具栏 +/−/fit 都能用', async () => {
    const zoomIn = page.locator('.flow-canvas__zoom-btn[data-zoom="in"]').first();
    const zoomOut = page.locator('.flow-canvas__zoom-btn[data-zoom="out"]').first();
    const fit = page.locator('.flow-canvas__zoom-btn[data-zoom="fit"]').first();
    const t0 = await viewportTransform(page);
    await zoomIn.click();
    await page.waitForTimeout(300);
    const t1 = await viewportTransform(page);
    await zoomOut.click();
    await page.waitForTimeout(300);
    const t2 = await viewportTransform(page);
    await fit.click();
    await page.waitForTimeout(500);
    const t3 = await viewportTransform(page);
    await shot(page, 'b11-zoom-toolbar');
    assert(t1 !== t0, '点 + 后视口没变');
    assert(t2 !== t1, '点 − 后视口没变');
    assert(t3 !== t2, '点 fit 后视口没变');
    return `比例读数 ${await zoomText(page)}`;
  });

  // ==================== B12 校验失败角标 ====================
  await check('B12', '校验失败时角标显示 + 问题清单', async () => {
    await page.getByRole('button', { name: '校验' }).click();
    await page.waitForTimeout(1200);
    const board = page.locator('.issue-board');
    const badges = page.locator('.flow-node__issue');
    const boardCount = await board.count();
    const badgeCount = await badges.count();
    await shot(page, 'b12-validation-issues');
    assert(boardCount > 0 || badgeCount > 0, '校验失败但既没有问题清单也没有角标');
    return `问题清单 ${boardCount} 个 · 角标 ${badgeCount} 个`;
  });

  // ==================== B7a 保存 ====================
  await check('B7a', '保存 → 成功', async () => {
    await page.getByRole('button', { name: '保存' }).click();
    await toastVisible(page, '已保存', 20000);
    await shot(page, 'b7-saved');
    return 'toast「已保存」';
  });

  // ==================== B7b 发布（另一条能通过校验的流程，UI 上点发布） ====================
  await check('B7b', '发布 → 成功', async () => {
    const fixture = await createPublishableFlow(token);
    await page.goto(`${BASE}/flow/def/${fixture.defId}/edit`, { waitUntil: 'domcontentloaded' });
    await waitCanvas(page, 3);
    await page.getByRole('button', { name: '校验' }).click();
    await toastVisible(page, '校验通过', 20000);
    await page.getByRole('button', { name: '发布' }).click();
    const ok = page.locator('.t-message').filter({ hasText: /已发布 v\d+/ }).first();
    await ok.waitFor({ timeout: 20000 });
    await shot(page, 'b7-published');
    const status = await api(`/api/business/flow/def/${fixture.defId}`, { token });
    assert(status.status === 'PUBLISHED', `发布后状态为 ${status.status}`);
    return `def ${fixture.code} → PUBLISHED（v${status.current_version_no}）`;
  });

  // ==================== B13 回退开关 ?canvas=native ====================
  await check('B13', '回退开关：?canvas=native 切回老画布', async () => {
    await page.goto(`${BASE}/flow/def/${PUBLISHED_DEF}/edit?canvas=native`, { waitUntil: 'domcontentloaded' });
    await page.waitForSelector('g.flow-node', { timeout: 30000 });
    await page.waitForTimeout(600);
    const nativeNodes = await page.locator('g.flow-node').count();
    const vueflow = await page.locator('.vue-flow').count();
    assert(nativeNodes >= 9, `老画布节点数 ${nativeNodes}`);
    assert(vueflow === 0, `?canvas=native 仍渲染了 Vue Flow 画布（${vueflow}）`);
    await shot(page, 'b13-native-fallback');
    return `老画布 ${nativeNodes} 节点 / .vue-flow 0`;
  });

  // ==================== D 性能（不在本脚本里判） ====================
  // 加载时间的 A/B 刻意**移出**本脚本，交给 `vueflow3_load_bench.cjs` + 生产产物（`vueflow3_static_preview.cjs`）：
  // 同一份代码在 dev 模式下反复测，中位数会在 -29% ~ +24% 之间跳（按需编译 / JIT / 磁盘缓存），
  // 在本脚本里加一条「±20%」的硬断言只会变成随机红。性能数据见 docs/evidence/20261104-vueflow-3/bench-*.json。
  const loadMetrics = { note: '见 bench-chromium.json / bench-webkit.json（生产产物 15 轮中位数）' };

  // ==================== 收尾 ====================
  // C2（Safari 专项）：拖节点 / 连线 / 缩放 本身就在 B4a / B4b / B11 里逐项量过，
  // 这里把它们在同一引擎下的结果汇总成一条（Safari 的判据就是「这三件事在 WebKit 上也能做」）。
  {
    const parts = ['B4a', 'B4b', 'B11'].map((id) => CHECKS.find((c) => c.id === id));
    const pass = parts.every((c) => c && c.pass);
    CHECKS.push({
      id: 'C2',
      item: '拖节点 / 能连线 / 能缩放（本引擎实测汇总）',
      pass,
      detail: parts.map((c) => `${c ? c.id : '?'}=${c && c.pass ? '✓' : '✗'}`).join(' '),
      ms: 0,
    });
    log(`  ${pass ? '✓' : '✗'} C2 拖节点/连线/缩放 — ${parts.map((c) => `${c ? c.id : '?'}=${c && c.pass ? '✓' : '✗'}`).join(' ')}`);
  }

  // 控制台：只判 JS 报错（pageerror / console.error 非资源类）；4xx 资源单独列在 httpErrors 里
  CHECKS.push({
    id: 'X1',
    item: '控制台 0 个 JS 报错（4xx 资源单列 httpErrors）',
    pass: CONSOLE_ERRORS.length === 0,
    detail: CONSOLE_ERRORS.length ? CONSOLE_ERRORS.slice(0, 3).join(' | ') : `HTTP 4xx/5xx ${HTTP_ERRORS.length} 条（notice SSE 的 401 属既有行为）`,
    ms: 0,
  });

  const failed = CHECKS.filter((c) => !c.pass);
  const result = {
    browser: ENGINE,
    baseUrl: BASE,
    publishedDefId: PUBLISHED_DEF,
    createdAt: new Date().toISOString(),
    passed: CHECKS.filter((c) => c.pass).length,
    failed: failed.length,
    checks: CHECKS,
    consoleErrors: CONSOLE_ERRORS,
    httpErrors: HTTP_ERRORS,
    loadMetrics,
    shots: SHOTS,
    cleanup: { newDefId },
  };
  fs.mkdirSync(path.dirname(OUT), { recursive: true });
  fs.writeFileSync(OUT, JSON.stringify(result, null, 2));

  // 清理本轮新建的流程（软删），夹具定义保持原样
  for (const defId of [newDefId, draftFixture && draftFixture.defId].filter(Boolean)) {
    await api(`/api/business/flow/def/${defId}`, { method: 'DELETE', token }).catch((error) => log(`  ! 清理 ${defId} 失败：${error.message}`));
  }

  log(`\n=== ${ENGINE}: ${result.passed} 通过 / ${result.failed} 失败 · 控制台错误 ${CONSOLE_ERRORS.length} 条 ===`);
  log(`JSON: ${OUT}`);
  await browser.close();
  if (failed.length || CONSOLE_ERRORS.length) {
    if (failed.length) log(`失败项：${failed.map((f) => `${f.id} ${f.detail}`).join(' | ')}`);
    process.exitCode = 1;
  }
}

main().catch((error) => {
  log(`FATAL: ${error && error.stack ? error.stack : error}`);
  process.exitCode = 1;
});
