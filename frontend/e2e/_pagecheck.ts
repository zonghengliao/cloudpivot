/**
 * 52 页全量 E2E 的「页面探针」（测试E / t_4c9d4e71）。
 *
 * # 为什么要探针而不是每页写一堆 expect
 *
 * 一盏页面要回答的问题只有 6 个（能开 / 表格渲染 / 列表接口自洽 / 按钮可达 /
 * 按钮有反应 / 空提交被拦）。这 6 个问题的**证据全部来自同一次页面访问**：
 * 导航一次、收集一次、交互一轮，然后让 6 条 `it` 分别对同一份现场断言。
 * 这样 56 个页面只产生 56 次导航（生产限流 300 req/60s，导航 6 遍必打满），
 * 同时每条 `it` 的断言仍然是具体的（对着现场里的数字断），不会退化成恒真。
 *
 * # 现场落盘
 *
 * 每个页面把 {@link Observation} 写成 `e2e-report/probe/<slug>.json`，
 * 截图 `e2e-report/probe/<slug>.png`。`docs/test-report-e2e.md` 由这些 JSON 汇总生成，
 * 报告里的每个 ✅/❌、每条「死按钮」都能指回一份现场文件。
 */
import { mkdirSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import type { Page } from '@playwright/test';
import { BASE_URL, assertNavigationOk } from './support';
import type { PageSpec } from './_catalog';

/**
 * **已知缺陷**：页面加载期每次都会出现的 4xx。
 *
 * 这些不算「本页坏了」，但**不许静默吞掉**：仍然逐条记进 `knownBadApi`，
 * 并在 `docs/test-report-e2e.md` 的「4xx 清单」里逐页列证 + 单独立一条专项断言
 * （`34-error-boundary.spec.ts` 末尾的「已知缺陷留证」），确保它不会被忘掉。
 *
 * 为什么不直接改成 `page.route` 屏蔽：屏蔽掉等于把缺陷从证据里删了——
 * 用户看不到「实时推送其实是断的」，报告也就没法回答「哪条接口报 401」。
 */
export const KNOWN_BAD_API: Array<{ match: RegExp; label: string; reason: string }> = [
  {
    match: /^\/api\/business\/notice\/stream$/,
    label: '通知 SSE',
    reason:
      'EventSource 不能自定义 Authorization 头，前端改用 ?token= 订阅；' +
      '但后端认证中间件只读 Authorization（alun-web 0.4.2 无 query-token 白名单，' +
      '仓库里也搜不到 NIK_AUTH_QUERY_TOKEN_PATHS）→ 每次页面加载都 401，实时推送从未连通，' +
      '靠 notice store 的 30s 轮询兜底。',
  },
];

/** 命中已知缺陷返回标签，否则 undefined */
function knownBadLabel(path: string, status: number): string | undefined {
  if (status < 400) {
    return undefined;
  }
  return KNOWN_BAD_API.find((k) => k.match.test(path))?.label;
}

/** 一条后端接口调用（列表接口会带上 total / 行数，用来验「总数与行数一致」） */
export interface ApiCall {
  method: string;
  /** 去掉 query 的路径 */
  path: string;
  status: number;
  /** `data.total`（分页结构才有） */
  total: number | null;
  /** `data.list.length` 或 `data.length`（数组结构） */
  rows: number | null;
  /** 请求里的 page_size（没有就 null） */
  pageSize: number | null;
  shape: 'paged' | 'array' | 'object' | 'unknown';
}

/** 页面上的一个按钮（内容区，不含侧栏/顶栏） */
export interface BtnInfo {
  text: string;
  disabled: boolean;
  /** class 里是否带 t-button--variant-* / theme，用来区分主次按钮 */
  cls: string;
  /** 可见（≥1px）——0 尺寸的按钮用户点不到 */
  visible: boolean;
  /** 只有图标没有文字（TDesign 的列设置 / 折叠 / 刷新图标按钮是真的这么写的） */
  icon: boolean;
  /** 在内容区按钮列表里的序号（同名按钮有多个时必须按序号点，否则永远只点到第一个） */
  idx?: number;
}

/** 页面现场（一次访问的全部证据） */
export interface Observation {
  key: string;
  path: string;
  kind: PageSpec['kind'];
  url: string;
  navStatus: number;
  title: string;
  /** 渲染的是 404 结果页（SPA 里 HTTP 仍是 200，必须看 DOM） */
  is404: boolean;
  /** 结果页编号：'403' / '404' / '500' / '' */
  resultCode: string;
  api: ApiCall[];
  /** 4xx/5xx 的接口（含非 /api 静态资源），页面加载期；已知缺陷已剔除 */
  badApi: string[];
  /** 命中 {@link KNOWN_BAD_API} 的已知缺陷（逐页留证，进报告「4xx 清单」） */
  knownBadApi: string[];
  pageErrors: string[];
  consoleErrors: string[];
  tableCount: number;
  rowCount: number;
  headers: string[];
  emptyText: string;
  totalText: string;
  contentText: string;
  buttons: BtnInfo[];
  /** 交互阶段结果（④⑤⑥ 的证据） */
  interact: {
    /** 试点的「安全按钮」（查询 / 刷新 / 导出 / 新建 …），点完都撤干净 */
    clicked: string[];
    /** 点击引发的接口请求条数 */
    requestDelta: number;
    /** 点击引发的 toast 文案 */
    toasts: string[];
    /** 点击后弹出的弹窗 / 抽屉数量 */
    dialogsOpened: number;
    /** 结构：按钮 → 观察到的反应 */
    reactions: Array<{ button: string; reaction: string }>;
    /** 点了没有任何反应的按钮（死按钮） */
    dead: Array<{ button: string; reason: string }>;
    /** 危险按钮（删除 / 提交 / 发布…）：只读断言，不真点，避免污染生产数据 */
    skippedDanger: string[];
    /** 空提交校验结果 */
    form: {
      /** 是否找到并打开了「新建 / 新增」弹窗 */
      opened: boolean;
      /** 弹窗里点了「确定 / 提交」 */
      submitted: boolean;
      /** 弹窗里所有按钮文案（诊断「没找到提交按钮」到底是文案不匹配还是根本没有） */
      dialogButtons?: string[];
      /** 入口是跳转型（表单在另一个路由）：落地 URL */
      navigatedUrl?: string;
      /** 跳转型入口的落地页是否有上传控件 */
      uploadControl?: boolean;
      /** 提交按钮被浮层遮挡、退化成 DOM click 时置 true（记在报告里） */
      forceClick?: boolean;
      /** 校验报错文案（空 → 没拦住） */
      validation: string[];
      /** 空提交后打出去的写请求（有值说明必填校验漏了） */
      writeRequests: string[];
      /** 弹窗是否被干净关掉（残留弹窗会挡住后续用例） */
      closed: boolean;
    };
    errors: string[];
  };
  /** 截图（相对 frontend/ 的路径） */
  screenshot: string;
  /** 采集耗时（ms） */
  ms: number;
  error?: string;
}

/** 按钮文案 → 危险动作（真点会改数据：只断言存在，不点） */
const DANGER = /删除|清空|停用|禁用|启用|重置密码|通过|拒绝|撤回|撤销|发布|结账|反审核|审核|注销|强制下线|生成|下账|入库|出库|确认收货|入库过账/;
/** 按钮文案 → 安全动作（点开就撤，纯读或只弹窗） */
const SAFE =
  /查询|搜索|重置|刷新|导出|导入|新建|新增|添加|创建|编辑|查看|详情|配置|分配|设置|上传|下载|展开|全部已读|标记已读|同步|预览|测试|展开全部|更多|返回|折叠|体检|重试|选择文件|下载模板|下载导入模板/;

function slug(text: string): string {
  return text
    .replace(/[^\w\u4e00-\u9fa5-]+/g, '_')
    .replace(/_+/g, '_')
    .slice(0, 40);
}

function probeDir(): string {
  const dir = resolve(process.cwd(), 'e2e-report/probe');
  mkdirSync(dir, { recursive: true });
  return dir;
}

/** 把现场写成 JSON（报告生成的唯一输入） */
export function writeObservation(obs: Observation): void {
  const file = resolve(probeDir(), `${slug(obs.path.replace(/\//g, '_'))}.json`);
  writeFileSync(file, JSON.stringify(obs, null, 2), 'utf8');
}

/** 去掉 baseURL 与 query，得到可断言的稳定路径 */
function normUrl(url: string): string {
  return url.replace(BASE_URL, '').split('?')[0];
}

function queryOf(url: string): URLSearchParams {
  const q = url.split('?')[1] ?? '';
  return new URLSearchParams(q);
}

/**
 * 页面加载期采集：导航 + 等渲染 + 抓接口/DOM。
 *
 * 全程不使用 `waitForTimeout` 硬等：导航后用 `waitForFunction` 等「布局出来 ∧
 * 主内容区有字 ∧ 表格/空态/结果页任一出现」，再 `networkidle` 兜一次（8s 上限，
 * 因为顶栏徽标是 30s 轮询，永远等不到真正的 idle）。
 */
export async function observePage(page: Page, spec: PageSpec): Promise<Observation> {
  const started = Date.now();
  const api: ApiCall[] = [];
  const badApi: string[] = [];
  const knownBadApi: string[] = [];
  const pageErrors: string[] = [];
  const consoleErrors: string[] = [];
  const pending: Array<Promise<void>> = [];

  page.on('response', (res) => {
    const full = res.url();
    const path = normUrl(full);
    const method = res.request().method();
    const status = res.status();
    if (status >= 400) {
      const known = knownBadLabel(path, status);
      const line = `${status} ${method} ${path}`;
      if (known) {
        knownBadApi.push(`${line}（已知缺陷：${known}）`);
      } else {
        badApi.push(line);
      }
    }
    if (!path.startsWith('/api/')) {
      return;
    }
    const call: ApiCall = { method, path, status, total: null, rows: null, pageSize: null, shape: 'unknown' };
    api.push(call);
    const size = queryOf(full).get('page_size');
    if (size) {
      call.pageSize = Number(size);
    }
    // 响应体解析是异步的：先排进 pending，等页面稳定后统一 await，避免竞态
    pending.push(
      res
        .json()
        .then((body) => {
          const data = body?.data;
          if (data && typeof data === 'object' && !Array.isArray(data) && 'total' in data) {
            call.shape = 'paged';
            call.total = Number((data as { total: unknown }).total ?? 0);
            const list = (data as { list?: unknown[] }).list;
            call.rows = Array.isArray(list) ? list.length : null;
          } else if (Array.isArray(data)) {
            call.shape = 'array';
            call.rows = data.length;
          } else if (data && typeof data === 'object') {
            call.shape = 'object';
            call.total = Number((data as { total?: unknown }).total ?? NaN) || null;
          }
        })
        .catch(() => {
          /* 非 JSON（导出流 / 空体）忽略 */
        }),
    );
  });
  page.on('pageerror', (err) => pageErrors.push(String(err).slice(0, 300)));
  page.on('console', (msg) => {
    if (msg.type() === 'error') {
      const text = msg.text().slice(0, 300);
      // 浏览器自己报的资源 404（如 favicon）与业务无关，单独归类但保留原文
      consoleErrors.push(text);
    }
  });

  let navStatus = 0;
  let error: string | undefined;
  try {
    const res = await page.goto(spec.path, { waitUntil: 'domcontentloaded' });
    navStatus = res?.status() ?? 0;
    assertNavigationOk(res, spec.path);
  } catch (e) {
    error = String(e).slice(0, 400);
    navStatus = -1;
  }

  // 等「布局 + 内容」就位：结果页（.t-result）与业务页（.basic-layout__aside）二选一
  await page
    .waitForSelector('.basic-layout__aside, .t-result, .result-page, .login-page', { timeout: 15_000 })
    .catch(() => undefined);
  await page
    .waitForFunction(
      () => {
        const content = document.querySelector('.basic-layout__content') ?? document.querySelector('.t-result, .result-page');
        if (!content) {
          return false;
        }
        if (document.querySelector('.t-table, .t-result, .result-page, .board, .gantt, .t-form, .help-center, .dict-page')) {
          return true;
        }
        return (content.textContent ?? '').trim().length > 20;
      },
      undefined,
      { timeout: 12_000 },
    )
    .catch(() => undefined);
  await page.waitForLoadState('networkidle', { timeout: 3_000 }).catch(() => undefined);
  // 表格若在骨架/加载态，等 loading 遮罩消失（TDesign 用 .t-loading）
  await page
    .waitForFunction(() => !document.querySelector('.t-table .t-loading, .t-loading--fullscreen'), undefined, {
      timeout: 4_000,
    })
    .catch(() => undefined);

  await Promise.all(pending);

  const pageTitle = await page.title();
  const dom = await page.evaluate(() => {
    const txt = (el: Element | null) => (el?.textContent ?? '').replace(/\s+/g, ' ').trim();
    const content = document.querySelector('.basic-layout__content') ?? document.querySelector('.t-result, .result-page') ?? document.body;
    // 行选择器实测：TDesign 1.19 的 <tr> **不带** class（`.t-table__row` 不存在），
    // 用 `thead th` 的表头行排除法比赌 class 稳。
    const rows = document.querySelectorAll('.t-table tbody tr');
    const ths = Array.from(document.querySelectorAll('.t-table thead th')).map((th) => txt(th));
    const emptyEl = document.querySelector('.t-table__empty, .t-table__empty-text, .t-empty, .t-empty__description');
    const pagination = document.querySelector('.t-pagination__total, .t-table__pagination .t-pagination__total');
    /** 真的看得见：有尺寸 ∧ 未被 display/visibility 隐藏（隐藏弹窗里的按钮 offsetParent 为 null） */
    const shown = (el: Element) => {
      const rect = el.getBoundingClientRect();
      return rect.width > 1 && rect.height > 1 && (el as HTMLElement).offsetParent !== null;
    };
    const btns = Array.from(content.querySelectorAll('button'))
      // 隐藏的弹窗 / 抽屉里的按钮用户点不到，不算「页面上的按钮」
      .filter((b) => !b.closest('.t-dialog, .t-drawer') || shown(b.closest('.t-dialog, .t-drawer') as Element))
      .map((b) => ({
        text: (txt(b) || b.getAttribute('aria-label') || b.getAttribute('title') || '').slice(0, 40),
        disabled: b.disabled || b.classList.contains('t-is-disabled'),
        cls: b.className.slice(0, 120),
        visible: shown(b),
        // 图标按钮（列设置 / 折叠 / 刷新）：TDesign 里就是「只有 svg/icon，没有文字」，
        // 这是合法写法，④ 不能用「没文案」一刀切成缺陷。
        icon: Boolean(b.querySelector('svg, .t-icon, i')),
      }))
    const result = document.querySelector('.t-result, .result-page');
    // 结果页有两套实现：TDesign 的 `.t-result` 和仓库自写的 `.result-page`（403/404/500 用它）。
    // 正文形如「403抱歉，您没有权限访问该页面」——数字和中文之间没有 \b，
    // 用带边界的正则会永远匹配不到（2026-10-31 实测踩过）。
    const resultCode = result ? (txt(result).match(/(403|404|500)/)?.[1] ?? '') : '';
    return {
      is404: Boolean(result && /404|页面不存在/.test(document.title + txt(result))),
      resultCode,
      tableCount: document.querySelectorAll('.t-table').length,
      rowCount: rows.length,
      headers: ths.filter(Boolean).slice(0, 12),
      emptyText: txt(emptyEl),
      totalText: txt(pagination as Element | null),
      contentText: txt(content).slice(0, 1200),
      buttons: btns.filter((b) => b.text || b.cls.includes('t-button--shape-rectangle')),
    };
  });

  const shotRel = `e2e-report/probe/${slug(spec.path.replace(/\//g, '_'))}.png`;
  await page.screenshot({ path: resolve(process.cwd(), shotRel), fullPage: false }).catch(() => undefined);

  return {
    key: spec.key,
    path: spec.path,
    kind: spec.kind,
    url: page.url().replace(BASE_URL, ''),
    navStatus,
    title: pageTitle,
    is404: dom.is404,
    resultCode: dom.resultCode,
    api,
    badApi,
    knownBadApi,
    pageErrors,
    consoleErrors,
    tableCount: dom.tableCount,
    rowCount: dom.rowCount,
    headers: dom.headers,
    emptyText: dom.emptyText,
    totalText: dom.totalText,
    contentText: dom.contentText,
    buttons: dom.buttons,
    interact: {
      clicked: [],
      requestDelta: 0,
      toasts: [],
      dialogsOpened: 0,
      reactions: [],
      dead: [],
      skippedDanger: [],
      form: { opened: false, submitted: false, validation: [], writeRequests: [], closed: true },
      errors: [],
    },
    screenshot: shotRel,
    ms: Date.now() - started,
    error,
  };
}

/** 现有**可见且有内容**的弹窗 / 抽屉 / toast 数量，外加「非浮层型反应」的指纹 */
async function overlayState(page: Page): Promise<{
  overlays: number;
  toasts: number;
  url: string;
  text: string;
  /** 内容区 DOM 指纹：展开/折叠、树展开、卡片增减都会改它（不弹窗不发请求的那种反应） */
  domSig: string;
  /** 可见的 TDesign 弹层（下拉 / 列设置 / 更多菜单都挂这里） */
  popups: number;
  /** aria-expanded=true 的元素数（树 / 折叠面板） */
  expanded: number;
}> {
  return page.evaluate(() => {
    const visible = (el: Element) => (el as HTMLElement).offsetParent !== null && el.getBoundingClientRect().height > 1;
    // 只算「有内容」的浮层。实测坑：TDesign 的部分抽屉/弹窗会常驻 DOM 且带 mask，
    // 空 body 时 height 仍 >1（supplier 页就有一个 data-testid=sup-drawer 的空抽屉），
    // 把它算进基线会让「点了没反应」被误判成「有反应」。
    const overlays = Array.from(document.querySelectorAll('.t-dialog, .t-drawer')).filter(
      (el) =>
        visible(el) &&
        ((el.textContent ?? '').replace(/\s+/g, '').length > 0 || el.querySelector('button, input, textarea, .t-form') !== null),
    );
    const content = document.querySelector('.basic-layout__content') ?? document.body;
    return {
      overlays: overlays.length,
      toasts: document.querySelectorAll('.t-message').length,
      url: location.href,
      text: (overlays[0]?.textContent ?? '').replace(/\s+/g, ' ').trim().slice(0, 60),
      domSig: `${content.querySelectorAll('*').length}:${(content.textContent ?? '').length}`,
      popups: Array.from(document.querySelectorAll('.t-popup')).filter(visible).length,
      expanded: document.querySelectorAll('[aria-expanded="true"]').length,
    };
  });
}

/** 可见且有内容浮层的 Playwright 选择器（同名多开时取最后一个挂载的） */
function contentOverlaySelector(): string {
  return '.t-dialog, .t-drawer';
}
void contentOverlaySelector;

/** 关掉所有**可见**弹窗（优先「取消 / 关闭」，其次关闭图标，最后 Esc） */
export async function closeOverlays(page: Page): Promise<void> {
  const visibleOverlays = () =>
    page.evaluate(
      () =>
        Array.from(document.querySelectorAll('.t-dialog, .t-drawer')).filter(
          (el) => (el as HTMLElement).offsetParent !== null && el.getBoundingClientRect().height > 1,
        ).length,
    );
  for (let i = 0; i < 5; i += 1) {
    const before = await visibleOverlays().catch(() => 0);
    if (before === 0) {
      return;
    }
    const cancel = page
      .locator('.t-dialog:visible, .t-drawer:visible')
      .last()
      .locator('button:has-text("取消"), button:has-text("关闭"), .t-dialog__close, .t-drawer__close')
      .first();
    if (await cancel.isVisible().catch(() => false)) {
      await cancel.click().catch(() => undefined);
    } else {
      await page.keyboard.press('Escape').catch(() => undefined);
    }
    await page
      .waitForFunction(
        (n) =>
          Array.from(document.querySelectorAll('.t-dialog, .t-drawer')).filter(
            (el) => (el as HTMLElement).offsetParent !== null && el.getBoundingClientRect().height > 1,
          ).length < n,
        before,
        { timeout: 3000 },
      )
      .catch(() => undefined);
    // 一轮没关掉任何东西就不要再空转：直接再按一次 Esc 兜底
    if ((await visibleOverlays().catch(() => 0)) >= before) {
      await page.keyboard.press('Escape').catch(() => undefined);
      await page.waitForTimeout(150);
    }
  }
}

/**
 * 交互阶段：逐个点「安全按钮」验反应（专项 2 死按钮清单的逐页版本），
 * 然后打开「新建 / 新增」弹窗做空提交校验（⑥）。
 *
 * 危险按钮（删除 / 停用 / 发布 …）**不点**：一点就改生产数据。它们只做
 * 「存在 + 可见 + 未禁用」的静态断言，并把文案记进 `skippedDanger`，报告里逐页列出。
 */
export async function interactPage(page: Page, spec: PageSpec, obs: Observation): Promise<void> {
  const fresh = await collectContentButtons(page);
  const danger = fresh.filter((b) => DANGER.test(b.text));

  obs.interact.skippedDanger = danger.map((b) => b.text).slice(0, 12);

  // 有些页面（字典管理）内容区只有一个图标按钮（列设置 / 展开），没有文案型安全按钮。
  // 这种页不能因为「没有安全按钮」就判 ⑤ 失败——退化成点那个图标按钮，同样验反应。
  // 禁用的按钮必须先排除：公告/消息中心没有未读时「全部已读」是灰的，
  // 点灰按钮当然没反应——那不是死按钮，是状态正确。
  const enabled = fresh.filter((b) => !b.disabled);
  const pool = enabled.filter((b) => SAFE.test(b.text) && !DANGER.test(b.text));
  const finalPool = pool.length > 0 ? pool : enabled.filter((b) => b.icon).slice(0, 1);

  let requestCount = 0;
  const reqListener = () => {
    requestCount += 1;
  };
  page.on('request', reqListener);
  // 「上传文件 / 选择文件」点开的是**原生文件选择器**：DOM 不变、不发请求，
  // 只从「没有可观察反应」的角度看就是死按钮。必须单独接这个信号。
  let fileChooserCount = 0;
  const onFileChooser = () => {
    fileChooserCount += 1;
  };
  page.on('filechooser', onFileChooser);

  for (const target of finalPool.slice(0, 8)) {
    try {
      const before = await overlayState(page);
      const beforeReqs = requestCount;
      const beforeChooser = fileChooserCount;
      // 按**序号**点，不按文案：一页里常有两个同名按钮（实测序列号页有 2 个「查询」，
      // 扫码查询在没输条码时点了什么都不做，而工具栏查询会发请求）。按文案取 .first()
      // 会把同一个按钮点两遍，第二个同名按钮永远没被验过 —— 那正是「死按钮漏报」的来源。
      const locator = page.locator('.basic-layout__content button, .result-page button, .t-result button').nth(target.idx ?? 0);
      const liveText = ((await locator.textContent({ timeout: 1_000 }).catch(() => '')) ?? '').replace(/\s+/g, ' ').trim();
      if (!(await locator.isVisible().catch(() => false)) || liveText !== target.text) {
        obs.interact.errors.push(`${target.text}#${target.idx}: 按钮序号已漂移（现文案「${liveText}」），本轮跳过`);
        continue;
      }
      await locator.click({ timeout: 2_000 }).catch((e: Error) => {
        obs.interact.errors.push(`${target.text}: 点击失败 ${String(e).slice(0, 120)}`);
      });
      obs.interact.clicked.push(target.text);

      // 反应窗口：等任一「可观察变化」，800ms 内出现即可（不 sleep，用 waitForFunction）
      const reaction = await page
        .waitForFunction(
          (prev) => {
            const visible = (el: Element) => (el as HTMLElement).offsetParent !== null && el.getBoundingClientRect().height > 1;
            const overlays = Array.from(document.querySelectorAll('.t-dialog, .t-drawer')).filter(
              (el) => (el as HTMLElement).offsetParent !== null && el.getBoundingClientRect().height > 1,
            ).length;
            const toasts = document.querySelectorAll('.t-message').length;
            const content = document.querySelector('.basic-layout__content') ?? document.body;
            const domSig = `${content.querySelectorAll('*').length}:${(content.textContent ?? '').length}`;
            const popups = Array.from(document.querySelectorAll('.t-popup')).filter(visible).length;
            const expanded = document.querySelectorAll('[aria-expanded="true"]').length;
            return (
              overlays > prev.overlays ||
              toasts > prev.toasts ||
              location.href !== prev.url ||
              domSig !== prev.domSig ||
              popups > prev.popups ||
              expanded !== prev.expanded
            );
          },
          before,
          { timeout: 800 },
        )
        .then(() => '弹窗/Toast/路由/DOM 变化')
        .catch(() => '');

      const after = await overlayState(page);
      const reqs = requestCount - beforeReqs;
      const chooserOpened = fileChooserCount > beforeChooser;
      obs.interact.requestDelta += reqs;
      obs.interact.dialogsOpened = Math.max(obs.interact.dialogsOpened, after.overlays);
      if (after.toasts > before.toasts) {
        obs.interact.toasts.push(target.text);
      }
      const kinds: string[] = [];
      if (reaction) {
        kinds.push(reaction);
      }
      if (after.overlays > before.overlays) {
        kinds.push('新弹窗/抽屉');
      }
      if (reqs > 0) {
        kinds.push(`请求 ×${reqs}`);
      }
      if (after.toasts > before.toasts) {
        kinds.push('Toast');
      }
      if (after.url !== before.url) {
        kinds.push('路由变化');
      }
      if (chooserOpened) {
        kinds.push('原生文件选择器');
      }
      if (after.popups > before.popups) {
        kinds.push('下拉/弹层');
      }
      if (after.expanded !== before.expanded) {
        kinds.push(after.expanded > before.expanded ? '展开' : '折叠');
      }
      if (after.domSig !== before.domSig && kinds.length <= 1) {
        kinds.push('DOM 变化');
      }
      if (kinds.length === 0) {
        obs.interact.dead.push({
          button: target.text || `(图标按钮#${target.idx ?? 0})`,
          reason: `#${target.idx} 点击后 800ms 内无弹窗/无请求/无 toast/无路由变化/无 DOM 变化`,
        });
      } else {
        obs.interact.reactions.push({ button: target.text || `(图标按钮#${target.idx ?? 0})`, reaction: kinds.join(' + ') });
      }
      await closeOverlays(page);
    } catch (e) {
      obs.interact.errors.push(`${target.text}: ${String(e).slice(0, 160)}`);
      await closeOverlays(page).catch(() => undefined);
    }
  }
  page.off('request', reqListener);
  page.off('filechooser', onFileChooser);

  if (spec.kind !== 'result') {
    await checkFormValidation(page, spec, obs);
  }
}

/** 内容区可见按钮（快照，交互中途 DOM 会变，所以每次重取） */
async function collectContentButtons(page: Page): Promise<BtnInfo[]> {
  return page.evaluate(() => {
    const content = document.querySelector('.basic-layout__content') ?? document.querySelector('.t-result, .result-page');
    if (!content) {
      return [];
    }
    // `idx` 是**未过滤**按钮列表里的下标，必须和 `interactPage` 里
    // `page.locator('.basic-layout__content button').nth(idx)` 用的是同一个列表，
    // 否则按序号点会点错人。
    return Array.from(content.querySelectorAll('button'))
      .map((b, idx) => {
        const rect = b.getBoundingClientRect();
        const inHiddenOverlay =
          b.closest('.t-dialog, .t-drawer') !== null &&
          (b.closest('.t-dialog, .t-drawer') as HTMLElement).getBoundingClientRect().height < 2;
        const visible = rect.width > 1 && rect.height > 1 && (b as HTMLElement).offsetParent !== null && !inHiddenOverlay;
        return {
          text: ((b.textContent ?? '').replace(/\s+/g, ' ').trim() || b.getAttribute('aria-label') || '').slice(0, 40),
          disabled: (b as HTMLButtonElement).disabled || b.classList.contains('t-is-disabled'),
          cls: b.className.slice(0, 120),
          visible,
          icon: Boolean(b.querySelector('svg, .t-icon, i')),
          idx,
        };
      })
      .filter((b) => b.visible);
  });
}

/**
 * ⑥ 表单必填校验：打开「新建 / 新增」弹窗 → 什么都不填直接提交 → 必须被拦住。
 *
 * 判据（实测口径，2026-10-31 用 `_probe-dom` 在物料管理页取的真实 DOM）：
 * TDesign 表单的字段级报错落在 `.t-form-item-tips.t-tips.t-is-error`（文案如「物料编码必填」），
 * 另有 toast 报错 `.t-message--error`。若空提交直接打出写请求（POST/PUT/PATCH/DELETE），
 * 记为 `writeRequests` —— 这是「必填校验漏了」的实证，报告里单列。
 */
export async function checkFormValidation(page: Page, spec: PageSpec, obs: Observation): Promise<void> {
  const form = obs.interact.form;
  if (spec.readOnly) {
    return;
  }
  const visibleOverlays = () =>
    page.evaluate(
      () =>
        Array.from(document.querySelectorAll('.t-dialog, .t-drawer')).filter(
          (el) => (el as HTMLElement).offsetParent !== null && el.getBoundingClientRect().height > 1,
        ).length,
    );
  const content = page.locator('.basic-layout__content, .result-page');
  const createBtn = content
    .locator('button')
    .filter({ hasText: /新建|新增|添加|创建|发起|发送通知|新建模板|新建物料/ })
    .first();
  const writes: string[] = [];
  const onReq = (req: import('@playwright/test').Request) => {
    const m = req.method();
    if ((m === 'POST' || m === 'PUT' || m === 'PATCH' || m === 'DELETE') && req.url().includes('/api/')) {
      writes.push(`${m} ${normUrl(req.url())}`);
    }
  };
  /** 校验文案选择器：字段级 tips、红框、全局 toast 都算「拦住了」 */
  const VALIDATION_SELECTOR = [
    '.t-form-item-tips.t-is-error',
    '.t-input__extra.t-is-error',
    '.t-textarea__extra.t-is-error',
    '.t-select__extra.t-is-error',
    '.t-form__controls.t-is-error',
    '.t-dialog .t-is-error',
    '.t-drawer .t-is-error',
    '.t-message',
    '.t-form__error',
    '.t-form__item.t-is-error',
  ].join(', ');

  /** 等校验文案 + 采集 */
  const collectValidation = async (): Promise<void> => {
    await page
      .waitForFunction((sel) => document.querySelectorAll(sel).length > 0, VALIDATION_SELECTOR, { timeout: 2_500 })
      .catch(() => undefined);
    form.validation = await page.evaluate(
      (sel) =>
        Array.from(document.querySelectorAll(sel))
          .map((el) => (el.textContent ?? '').replace(/\s+/g, ' ').trim())
          .filter((text) => text.length > 0)
          .slice(0, 6),
      VALIDATION_SELECTOR,
    );
    // 兜底：有的表单把报错写在自定义节点里（类名不带 is-error）。扫可见浮层正文里的
    // 「必填 / 不能为空 / 请输入 / 请选择」——这仍然是对「用户真的看到了什么」的断言，
    // 不是恒真：没有报错文案的页面这里就是空数组。
    if (form.validation.length === 0) {
      form.validation = await page.evaluate(() => {
        const vis = (el: Element) => (el as HTMLElement).offsetParent !== null && el.getBoundingClientRect().height > 1;
        const texts: string[] = [];
        for (const el of Array.from(document.querySelectorAll('.t-dialog, .t-drawer')).filter(vis)) {
          for (const line of (el.textContent ?? '').split(/\s{2,}|\n/)) {
            const t = line.replace(/\s+/g, ' ').trim();
            if (t && /(必填|不能为空|请输入|请填写|请选择|长度需为)/.test(t) && t.length < 60) {
              texts.push(t);
            }
          }
        }
        return Array.from(new Set(texts)).slice(0, 6);
      });
    }
  };

  page.on('request', onReq);
  try {
    await closeOverlays(page);

    // ── 形态 A：入口是「跳转到表单页」（导入任务页的「发起导入」→ /business/impexp/import） ──
    if (spec.formMode === 'navigate') {
      const beforeUrl = page.url();
      await createBtn.click({ timeout: 3_000 }).catch(() => undefined);
      await page
        .waitForFunction((prev) => location.href !== prev, beforeUrl, { timeout: 5_000 })
        .catch(() => undefined);
      form.navigatedUrl = page.url().replace(BASE_URL, '');
      form.opened = Boolean(form.navigatedUrl) && form.navigatedUrl.split('?')[0] !== spec.path;
      // 落地页必须给出上传控件，否则「点了入口」等于什么也没发生
      form.uploadControl = await page
        .locator('input[type=file], .t-upload, button:has-text("选择文件")')
        .count()
        .then((n) => n > 0)
        .catch(() => false);
      form.writeRequests = writes.slice(0, 5);
      return;
    }

    // ── 形态 B：页面本身就是表单（消息发送），提交按钮在页面上 ──
    if (spec.formMode === 'page') {
      const submit = content
        .locator('button')
        .filter({ hasText: /^\s*(发送|提交|保存|确定|确认)\s*$/ })
        .last();
      if (!(await submit.isVisible().catch(() => false))) {
        return;
      }
      form.opened = true;
      await submit.click({ timeout: 2_000 }).catch(() => undefined);
      form.submitted = true;
      await collectValidation();
      form.writeRequests = writes.slice(0, 5);
      return;
    }

    // ── 形态 C（默认）：点「新建 / 新增」弹弹窗 / 抽屉 ──
    if (!(await createBtn.isVisible().catch(() => false))) {
      return;
    }
    await createBtn.click({ timeout: 2_000 }).catch(() => undefined);
    // 只认**可见且带输入控件**的浮层，而且取**最后**一个：
    // 实测坑（2026-10-31 现场取证）：供应商页常驻一个空的 `t-drawer[data-testid=sup-drawer]`；
    // 任务列表页还会残留上一个「登记」弹窗——按 `.first()` 会选中它们，于是 inputs=0 / 提交按钮错位。
    const candidates = page.locator('.t-dialog:visible, .t-drawer:visible');
    const n = await candidates.count().catch(() => 0);
    let dialog = candidates.last();
    // 优先选「标题/开头就是新建/新增/添加」的浮层：任务列表页会残留上一个「登记工时」
    // 弹窗，只按「最后一个有 input 的」会点错人（实测 2026-10-31，dialogButtons 里混进了
    // 「登记」）。没有命中的再退回「最后一个带输入的浮层」。
    const CREATE_TITLE = /^\s*(新建|新增|添加|创建|编辑)/;
    for (let i = n - 1; i >= 0; i -= 1) {
      const cand = candidates.nth(i);
      const hasInput = (await cand.locator('input, textarea').count().catch(() => 0)) > 0;
      if (!hasInput) {
        continue;
      }
      const head = (await cand.innerText().catch(() => '')).replace(/\s+/g, ' ').trim().slice(0, 30);
      if (CREATE_TITLE.test(head)) {
        dialog = cand;
        break;
      }
      dialog = cand;
    }
    if (!(await dialog.isVisible({ timeout: 2_000 }).catch(() => false))) {
      return;
    }
    form.opened = true;
    form.dialogButtons = await dialog
      .locator('button')
      .evaluateAll((els) => els.map((el) => (el.textContent ?? '').replace(/\s+/g, ' ').trim()).filter(Boolean).slice(0, 10));
    const inputs = await dialog.locator('input, textarea').count();
    if (inputs === 0) {
      // 不是表单弹窗（确认框 / 纯说明），交给报告人工看
      return;
    }
    // 提交按钮文案五花八门（确定 / 保存 / 创建 / 创建并进入编辑器 / 立即导入 …），
    // 用「包含」而不是全等；纯「取消 / 关闭 / 试渲染」绝不点。
    const submit = dialog
      .locator('button')
      .filter({ hasText: /确定|确认|提交|保存|创建|新建|发起|下一步|导入|导出|发布|生成/ })
      .filter({ hasNotText: /取消|关闭|重置|试渲染|预览|返回/ })
      .first();
    if (!(await submit.isVisible().catch(() => false))) {
      return;
    }
    // 有些页叠了一个常驻的**空抽屉**（.t-drawer--right，比如新建项目页），
    // 它会 intercept pointer events → 正常 click 3s 超时、校验永远不会触发。
    // 命中这种情况记 `forceClick` 并退化成 DOM click（要验的是校验逻辑，不是层级遮挡）。
    try {
      await submit.click({ timeout: 3_000 });
    } catch {
      form.forceClick = true;
      await submit.evaluate((el: HTMLElement) => el.click()).catch(() => undefined);
    }
    form.submitted = true;
    await collectValidation();
    form.writeRequests = writes.slice(0, 5);
  } catch (e) {
    obs.interact.errors.push(`表单校验: ${String(e).slice(0, 160)}`);
  } finally {
    page.off('request', onReq);
    await closeOverlays(page).catch(() => undefined);
    form.closed = (await visibleOverlays().catch(() => 1)) === 0;
  }
}
