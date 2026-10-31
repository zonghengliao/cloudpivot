import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
/**
 * E2E 公共支撑：地址 / 账号 / 登录 / 真实后端 API 预置与清理。
 *
 * 红线：**不打桩、不 mock**。所有 API 都是真打生产后端（同样的 `http://<host>/api/...`），
 * 页面里的每个断言都来自浏览器真实点出来的 DOM / 网络结果。
 * API 只用于两件事：① 给交互测试预置数据（如看板要拖的卡片）；② 用例结束后的清理。
 */
import { expect, request, type APIRequestContext, type Page } from '@playwright/test';

/** 被测站点（CI 用 `E2E_BASE_URL` 覆盖） */
export const BASE_URL = process.env.E2E_BASE_URL ?? 'http://172.18.10.144';
/** 测试账号（CI 用 `E2E_USERNAME` / `E2E_PASSWORD` 覆盖） */
export const ACCOUNT = {
  username: process.env.E2E_USERNAME ?? 'laoliu_test',
  password: process.env.E2E_PASSWORD ?? 'Test@12345',
};
/** 本次运行的数据戳：所有测试数据的名字里都带它，便于定位与清理 */
export const RUN = `e2e${Date.now().toString(36)}`;

/**
 * UI 登录：真实填表 + 点登录按钮。
 *
 * 生产后端对连续登录有节流，且登录请求偶发比 30s 慢；这里做「失败重来」的重试，
 * 但不绕过真实表单 —— 每次重试都是重新打开 /login 再点一次。
 */
export async function login(page: Page): Promise<void> {
  let lastError = '';
  for (let attempt = 1; attempt <= 3; attempt += 1) {
    await page.goto('/login');
    await page.waitForSelector("input[placeholder='请输入用户名']");
    await page.fill("input[placeholder='请输入用户名']", ACCOUNT.username);
    await page.fill("input[placeholder='请输入密码']", ACCOUNT.password);
    await page.click('button.t-button--theme-primary');
    try {
      await page.waitForURL(/\/(dashboard|change-password)/, { timeout: 30_000 });
      return;
    } catch {
      lastError = (await page.locator('.t-alert').first().innerText().catch(() => '')) || '未跳转到工作台';
      await page.waitForTimeout(1500 * attempt);
    }
  }
  throw new Error(`登录失败（已重试 3 次）：${lastError}`);
}

/** 读 `globalSetup` 存下的共享令牌（读不到返回空串，调用方回退到真登录） */
export function readSharedToken(): string {
  try {
    return readFileSync(resolve(process.cwd(), 'e2e/.auth/token.txt'), 'utf8').trim();
  } catch {
    return '';
  }
}

/**
 * 已带登录态的用例入口：直接进目标页；万一被踢回登录页（令牌过期）再走一次真实表单登录。
 *
 * # 两种「不是用例的错」的失败，这里提前翻译成人话
 *
 * - **导航返回 404**：多半是前端 dist 正在被替换（发布窗口里 `mv dist dist.old` 与
 *   `mv dist.new dist` 之间的空窗，nginx 拿不到 `index.html` 就直接 404）。不翻译的话
 *   症状是 `.basic-layout__aside` 15s 不可见，看不出跟发布有关（2026-10-30 12:54:55 就撞过）。
 * - **导航返回 429**：生产限流（300 req/60s，按 IP）。不翻译的话症状是用例里某个断言干等超时。
 */
export async function openAuthed(page: Page, path: string): Promise<void> {
  watchRateLimit(page);
  assertNavigationOk(await page.goto(path), path);
  if (/\/login/.test(page.url())) {
    await login(page);
    assertNavigationOk(await page.goto(path), path);
  }
  await expect(page.locator('.basic-layout__aside')).toBeVisible();
}

/**
 * 导航响应体检：把「发布窗口的 404」「限流 429」「网关 5xx」直接抛成可读错误。
 *
 * SPA 深链的响应就是 `index.html`，所以 200 是正常值；非 200 一律先怀疑环境而不是用例。
 */
export function assertNavigationOk(
  response: import('@playwright/test').Response | null,
  path: string,
): void {
  if (!response) {
    return; // about:blank / data: 之类没有主文档响应的导航
  }
  const status = response.status();
  if (status < 400) {
    return;
  }
  if (status === 404) {
    throw new Error(
      `打开 ${path} 返回 404：大概率是前端 dist 正在被替换（发布窗口的空窗期），不是用例问题。` +
        '等发布结束（可「ls -ld /opt/cloudpivot/frontend/dist」看 mtime 是否稳定）后重跑，见 e2e/README.md「已知前提 → 发布窗口」。',
    );
  }
  if (status === 429) {
    throw new Error(
      `打开 ${path} 命中生产限流 429（300 req/60s，按 IP）：冷却 ≥60s 后再单跑，别连跑、别并发、` +
        `别和别人的跑重叠，见 e2e/README.md「限流与连跑间隔」。`,
    );
  }
  throw new Error(`打开 ${path} 返回 ${status}：后端/网关异常，先看 systemctl status cloudpivot 与 journalctl -u cloudpivot。`);
}

/** 每个 page 命中的 429（用来给失败原因「加一句解释」） */
const rateLimitHits = new WeakMap<Page, string[]>();

/**
 * 装 429 监听（每个 page 只装一次）。
 *
 * 命中时不仅记账，还会 `console.warn` 一行 `[e2e] 生产限流 429: <METHOD> <path>`，
 * **CI 日志里直接能看到**——比「某个 toast 20s 没出现」好定位得多。
 */
export function watchRateLimit(page: Page): void {
  if (rateLimitHits.has(page)) {
    return;
  }
  const hits: string[] = [];
  rateLimitHits.set(page, hits);
  page.on('response', (response) => {
    if (response.status() !== 429) {
      return;
    }
    const where = `${response.request().method()} ${response.url().replace(BASE_URL, '')}`;
    hits.push(where);
    console.warn(`[e2e] 生产限流 429: ${where}（300 req/60s 按 IP，冷却 ≥60s 后单跑）`);
  });
}

/** 取该 page 命中过的 429（诊断用；空数组 = 没被限流过） */
export function rateLimitHitsOf(page: Page): string[] {
  return [...(rateLimitHits.get(page) ?? [])];
}

/** TDesign 消息条文案（成功 / 失败提示都在这里） */
export function toast(page: Page, text: string | RegExp) {
  return page.locator('.t-message').filter({ hasText: text }).first();
}

/** 真实后端 API 客户端（登录态复用测试账号） */
export interface Api {
  ctx: APIRequestContext;
  createTask(body: Record<string, unknown>): Promise<{ id: string; task_no: string }>;
  /** 任务列表里的一行（列表接口只下发 TaskCard 字段，没有 exec_mode） */
  listTasks(keyword?: string, pageSize?: number): Promise<TaskRow[]>;
  /** 任务详情（`exec_mode` 只在这里下发：`HUMAN` / `AI`） */
  getTaskDetail(id: string): Promise<Record<string, unknown>>;
  /** AI 产出文件数（`GET /business/task/{id}/ai-workspace-files` 的 total） */
  aiFilesTotal(id: string): Promise<number>;
  deleteTask(id: string): Promise<void>;
  createFlowDef(body: Record<string, unknown>): Promise<{ id: string; version_id: string }>;
  saveFlowGraph(vid: string, nodes: unknown[], edges: unknown[]): Promise<void>;
  publishFlowDef(id: string): Promise<void>;
  deleteFlowDef(id: string): Promise<void>;
  createInstance(defId: string, title: string): Promise<{ instance_id: string; instance_status: string }>;
  getInstance(id: string): Promise<{ instance: { id: string; status: string } }>;
  /** 撤回在跑的实例（清理用：有在跑实例时流程定义删不掉） */
  withdrawInstance(id: string, reason?: string): Promise<void>;
  dispose(): Promise<void>;
}

/**
 * 任务列表行（`GET /business/task/page` 的 `TaskCardRes` 子集）。
 *
 * 注意**没有** `exec_mode`：列表接口不下发这个字段，要判断 AI / 人工任务必须再拉详情。
 */
export interface TaskRow {
  id: string;
  task_no: string;
  title: string;
  status: string;
  child_count: number;
  parent_id: string | null;
}

/**
 * 真实后端 API 客户端。
 *
 * 令牌优先复用 `globalSetup` 存下的那份（`.auth/token.txt`）——后端登录接口有
 * 10 次 / 60 秒 的限流，用例里逐个登录会被 429。只有拿不到令牌时才真登一次。
 */
export async function newApi(): Promise<Api> {
  const bare = await request.newContext({ baseURL: BASE_URL });
  let token = readSharedToken();
  if (!token) {
    const res = await bare.post('/api/system/auth/login', { data: ACCOUNT });
    expect(res.status(), 'API 登录应返回 200（429 表示触发了登录限流）').toBe(200);
    token = (await res.json())?.data?.access_token;
    await bare.dispose();
  }
  expect(token, 'API 登录应拿到 access_token').toBeTruthy();
  const headers = { Authorization: `Bearer ${token}` };
  // 令牌必须挂在**context 级别**：专项用例里有大量 `api.ctx.get(...)` 直接调用
  // （拉菜单树 / 造账号 / 删夹具），只给 `call()` 传 header 会让这些请求 401
  // （2026-10-31 实测：30-menu-alignment 报「菜单接口应 200 / Received: 401」就是这个坑）。
  const ctx = await request.newContext({ baseURL: BASE_URL, extraHTTPHeaders: headers });

  /** 统一拆包：非 0 业务码直接抛出后端 msg，失败原因一眼可见 */
  const call = async (method: 'get' | 'post' | 'put' | 'delete', url: string, data?: unknown) => {
    const res = await ctx[method](url, { headers, data });
    const json = await res.json().catch(() => ({ code: -1, msg: `<非 JSON 响应 ${res.status()}>` }));
    if (json.code !== 0) {
      throw new Error(`API ${method.toUpperCase()} ${url} 失败：${res.status()} / ${json.code} / ${json.msg}`);
    }
    return json.data;
  };

  return {
    ctx,
    createTask: (body) => call('post', '/api/business/task', body),
    listTasks: async (keyword, pageSize = 200) => {
      const query = new URLSearchParams({ page: '1', page_size: String(pageSize) });
      if (keyword) {
        query.set('keyword', keyword);
      }
      const data = await call('get', `/api/business/task/page?${query.toString()}`);
      return (data.list ?? []) as TaskRow[];
    },
    getTaskDetail: (id) => call('get', `/api/business/task/${id}`),
    aiFilesTotal: async (id) => {
      const data = await call('get', `/api/business/task/${id}/ai-workspace-files`);
      return Number(data.total ?? data.list?.length ?? 0);
    },
    deleteTask: async (id) => {
      await call('delete', `/api/business/task/${id}`);
    },
    createFlowDef: (body) => call('post', '/api/business/flow/def', body),
    saveFlowGraph: async (vid, nodes, edges) => {
      await call('put', `/api/business/flow/version/${vid}/graph`, { nodes, edges });
    },
    publishFlowDef: async (id) => {
      await call('post', `/api/business/flow/def/${id}/publish`);
    },
    deleteFlowDef: async (id) => {
      await call('delete', `/api/business/flow/def/${id}`);
    },
    createInstance: (defId, title) => call('post', '/api/business/flow/instance', { def_id: defId, title }),
    getInstance: async (id) => call('get', `/api/business/flow/instance/${id}`),
    withdrawInstance: async (id, reason) => {
      await call('post', `/api/business/flow/instance/${id}/withdraw`, { reason: reason ?? 'E2E 用例清理' });
    },
    dispose: () => ctx.dispose(),
  };
}

/** 取元素中心点（拖拽 / 连线都要精确坐标） */
export async function centerOf(locator: import('@playwright/test').Locator): Promise<{ x: number; y: number }> {
  const box = await locator.boundingBox();
  if (!box) {
    throw new Error('元素不可见，取不到坐标');
  }
  return { x: box.x + box.width / 2, y: box.y + box.height / 2 };
}

/**
 * 用**真实鼠标事件**做一次 HTML5 原生拖拽（`draggable` + `dragstart/dragover/drop`）。
 *
 * 为什么不用 `locator.dragTo()`：拖拽目标元素是 `draggable` 时，Chromium 会把
 * mouseMoved 转成 drag 事件；`dragTo` 只做**一次性**位移，浏览器在这一跳里拿不到
 * 中间轨迹，`drop` 会落回**起点元素**（实测：`dragstart`/`dragover`/`drop` 的
 * target 全是源卡片自己，列上的 `handleDrop` 因为 `from === to` 直接 return，
 * 卡片原地不动、也没有任何报错）。分多步移动 + 落点前补两小步，drop 才会落到目标元素上。
 *
 * @param from 被拖元素（`draggable` 的那个）
 * @param to   落点（屏幕坐标；用 `await dropPoint(locator)` 取，别用待拖元素）
 */
export async function mouseDrag(
  page: Page,
  from: import('@playwright/test').Locator,
  to: { x: number; y: number },
): Promise<void> {
  const start = await centerOf(from);
  await page.mouse.move(start.x, start.y);
  await page.mouse.down();
  await page.mouse.move(to.x, to.y, { steps: 25 });
  await page.waitForTimeout(150);
  // 手指/鼠标「停稳」后浏览器才会补最后一次 dragover，drop 才认得这个落点
  await page.mouse.move(to.x + 2, to.y + 2, { steps: 3 });
  await page.mouse.up();
}

/** 容器内一个可放下的落点（避开容器顶部的标题栏 / 列头） */
export async function dropPoint(locator: import('@playwright/test').Locator, offsetY = 80): Promise<{ x: number; y: number }> {
  const box = await locator.boundingBox();
  if (!box) {
    throw new Error('落点容器不可见，取不到坐标');
  }
  return { x: box.x + box.width / 2, y: box.y + offsetY };
}

// ======================================================================
// 看板现场探针（偶发红定位用）
// ======================================================================

/**
 * 看板「现场」探针。
 *
 * # 解决什么
 *
 * 流程3 的拖拽偶发红过一次（后端 200、页面没反映），当时只有 `toHaveCount` 的超时信息，
 * 没留下「那时页面上到底弹了什么 toast、两列各有哪些卡」。这个探针把三样东西攒下来：
 * ① 浏览器 console 的 error/warning（Vue 运行时错误也在这里）；
 * ② 与 `business/task` 相关的接口调用（方法 / 路径 / 状态 / 是否失败）；
 * ③ 出事那一刻两列的卡片标题与页面上的 toast 文案。
 *
 * 用法：用例开头 `const probe = installBoardProbe(page)`，把关键断言塞进
 * `withDiagnostics(probe, testInfo, '标签', async () => { ... })`；断言失败时它会自动
 * `testInfo.attach` 一份 JSON（`e2e-report/` 里可下载），控制台也会打一份。
 */
export interface BoardProbe {
  /** console 的 error / warning 记录 */
  console: Array<{ at: number; type: string; text: string }>;
  /** `business/task` 相关请求 */
  net: Array<{ at: number; method: string; path: string; status?: number; failed?: string }>;
  /** 采样当前 DOM：两列卡片标题 + toast 文案 + 列计数 */
  snapshot(): Promise<{
    todo: string[];
    doing: string[];
    todoCount: string;
    doingCount: string;
    toasts: string[];
  }>;
}

/** 装在某个 page 上（页面导航不会丢，监听挂在 page 上） */
export function installBoardProbe(page: Page): BoardProbe {
  const started = Date.now();
  const probe: BoardProbe = {
    console: [],
    net: [],
    async snapshot() {
      return page.evaluate(() => {
        const cards = (status: string) =>
          Array.from(document.querySelectorAll(`.board .col[data-status="${status}"] .board-card`)).map((el) =>
            (el.querySelector('.board-card__title')?.textContent ?? '').trim(),
          );
        const count = (status: string) =>
          (document.querySelector(`.board .col[data-status="${status}"] .col__count`)?.textContent ?? '').trim();
        return {
          todo: cards('TODO'),
          doing: cards('DOING'),
          todoCount: count('TODO'),
          doingCount: count('DOING'),
          toasts: Array.from(document.querySelectorAll('.t-message')).map((el) =>
            (el.textContent ?? '').replace(/\s+/g, ' ').trim(),
          ),
        };
      });
    },
  };
  page.on('console', (msg) => {
    if (msg.type() !== 'error' && msg.type() !== 'warning') return;
    probe.console.push({ at: Date.now() - started, type: msg.type(), text: msg.text().slice(0, 500) });
  });
  page.on('pageerror', (error) => {
    probe.console.push({ at: Date.now() - started, type: 'pageerror', text: String(error).slice(0, 500) });
  });
  page.on('response', (response) => {
    const url = response.url();
    if (!url.includes('/api/business/task')) return;
    probe.net.push({
      at: Date.now() - started,
      method: response.request().method(),
      path: url.replace(BASE_URL, ''),
      status: response.status(),
    });
  });
  page.on('requestfailed', (request) => {
    if (!request.url().includes('/api/')) return;
    probe.net.push({
      at: Date.now() - started,
      method: request.method(),
      path: request.url().replace(BASE_URL, ''),
      failed: request.failure()?.errorText ?? 'failed',
    });
  });
  return probe;
}

/**
 * 跑一段断言，失败时把探针现场落盘 + attach 到报告 + 打一份到控制台，再把原始错误抛出去。
 *
 * 落盘位置：`test-results/diag/<label>-<时间戳>.json`（`test-results/` 每次运行会被
 * Playwright 清空，所以**这次红的现场一定还热着**，直接 `cat` 就能看）。
 *
 * 只加「诊断」，不改「判定」：断言逻辑原样执行，失败照旧红。
 */
export async function withDiagnostics<T>(
  probe: BoardProbe,
  testInfo: import('@playwright/test').TestInfo,
  label: string,
  body: () => Promise<T>,
): Promise<T> {
  try {
    return await body();
  } catch (error) {
    let dom: Awaited<ReturnType<BoardProbe['snapshot']>> | { error: string };
    try {
      dom = await probe.snapshot();
    } catch (snapshotError) {
      dom = { error: String(snapshotError) };
    }
    const payload = { label, at: new Date().toISOString(), url: testInfo.project.name, dom, console: probe.console, net: probe.net };
    const json = JSON.stringify(payload, null, 2);
    // ① 落盘（最可靠：不受 reporter 影响）
    try {
      const dir = resolve(process.cwd(), 'test-results/diag');
      mkdirSync(dir, { recursive: true });
      writeFileSync(resolve(dir, `${label}-${Date.now()}.json`), json, 'utf8');
    } catch {
      // 落盘失败也不该盖掉原来的断言错误
    }
    // ② attach 到报告（html reporter 里可直接下载）
    await testInfo.attach(`diag-${label}.json`, { body: json, contentType: 'application/json' });
    // ③ 控制台留一份（CI 日志里能直接搜 `[diag:`）
    console.error(`\n[diag:${label}] ${JSON.stringify(payload)}`);
    throw error;
  }
}
