#!/usr/bin/env node
/**
 * VUE-FLOW-3 性能对照：编辑器页加载时间（默认 Vue Flow vs ?canvas=native）。
 *
 * 为什么要单独一个脚本、并且要重复多次：
 *   · 单次 goto 的抖动很大（dev 模式按需编译 / JIT / 磁盘缓存），一次采样判「±20%」纯靠运气；
 *   · 这里每个引擎跑 N 次（默认 5），先各预热一次，再交替顺序采样，最后取**中位数**比较；
 *   · 同时记录 `resource` 里加载的脚本字节数，证明 `@vue-flow/*` 只在编辑器路由加载、
 *     不会出现在首屏（首页 dashboard 的资源清单另记一份）。
 *
 * 用法：
 *   node scripts/verify/vueflow3_load_bench.cjs --browser chromium \
 *     --base-url http://127.0.0.1:5188 --out docs/evidence/20261104-vueflow-3/bench-chromium.json
 */
'use strict';

const fs = require('node:fs');
const path = require('node:path');

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
const ENGINE = (args.browser || 'chromium').toLowerCase();
const OUT = args.out || `docs/evidence/20261104-vueflow-3/bench-${ENGINE}.json`;
const ROUNDS = Number(args.rounds || 5);
const DEF = args['def-id'] || '20000000-0000-4000-8000-0000000a0182';
const USER = args.user || 'laoliu_test';
const PASSWORD = args.password || 'Test@12345';
const TOKEN_FILE = args['token-file'] || `/tmp/vf3-bench-token-${ENGINE}.json`;

async function api(pathname, { method = 'GET', body, token } = {}) {
  const res = await fetch(`${BASE}${pathname}`, {
    method,
    headers: { 'Content-Type': 'application/json', ...(token ? { Authorization: `Bearer ${token}` } : {}) },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const json = await res.json().catch(() => ({}));
  if (json.code !== 0) throw new Error(`${method} ${pathname} → code=${json.code} ${json.msg || ''}`);
  return json.data;
}

async function token() {
  try {
    const cached = JSON.parse(fs.readFileSync(TOKEN_FILE, 'utf-8'));
    const payload = JSON.parse(Buffer.from(String(cached.access_token).split('.')[1], 'base64url').toString('utf-8'));
    if (payload.exp * 1000 - Date.now() > 120000) return cached;
  } catch {
    /* 重新登录 */
  }
  const fresh = await api('/api/system/auth/login', { method: 'POST', body: { username: USER, password: PASSWORD } });
  fs.writeFileSync(TOKEN_FILE, JSON.stringify(fresh, null, 2));
  return fresh;
}

function median(values) {
  const sorted = [...values].sort((a, b) => a - b);
  const mid = Math.floor(sorted.length / 2);
  return sorted.length % 2 ? sorted[mid] : (sorted[mid - 1] + sorted[mid]) / 2;
}

(async () => {
  const pw = loadPlaywright();
  const engine = ENGINE === 'webkit' || ENGINE === 'safari' ? pw.webkit : pw.chromium;
  const browser = await engine.launch();
  const context = await browser.newContext({ viewport: { width: 1600, height: 1000 }, locale: 'zh-CN', timezoneId: 'Asia/Shanghai' });
  const login = await token();
  await context.addInitScript(
    ({ a, r }) => {
      localStorage.setItem('x_access_token', a);
      localStorage.setItem('x_refresh_token', r);
      localStorage.setItem('cp-onboarding-done', '1');
    },
    { a: login.access_token, r: login.refresh_token },
  );
  const page = await context.newPage();

  const urls = {
    vueflow: `${BASE}/flow/def/${DEF}/edit`,
    native: `${BASE}/flow/def/${DEF}/edit?canvas=native`,
  };

  /** 量一次：从 goto 到画布出现首个节点 */
  async function measure(label) {
    const t0 = Date.now();
    await page.goto(urls[label], { waitUntil: 'domcontentloaded' });
    await page.waitForSelector('.vue-flow__node, g.flow-node', { timeout: 30000 });
    const ms = Date.now() - t0;
    // 该页加载的脚本字节数（transferSize 为 0 时回退 encodedBodySize）
    const scripts = await page.evaluate(() =>
      performance
        .getEntriesByType('resource')
        .filter((e) => /\.(js|mjs|ts|vue)(\?|$)/.test(e.name))
        .reduce((acc, e) => acc + (e.transferSize || e.encodedBodySize || 0), 0),
    );
    return { ms, scriptBytes: scripts };
  }

  // 预热（把 dev 模式的按需编译、JIT 都跑热，避免第一次采样被编译时间污染）
  await measure('vueflow');
  await measure('native');

  const samples = { vueflow: [], native: [] };
  for (let i = 0; i < ROUNDS; i += 1) {
    // 交替顺序：奇偶轮颠倒，抵消「后测的占便宜（缓存更热）」
    const order = i % 2 === 0 ? ['vueflow', 'native'] : ['native', 'vueflow'];
    for (const label of order) {
      const r = await measure(label);
      samples[label].push(r);
      process.stdout.write(`  round ${i + 1} ${label}: ${r.ms}ms / scripts ${(r.scriptBytes / 1024).toFixed(0)}KB\n`);
    }
  }

  // 首屏（首页 dashboard）加载的脚本字节：证明 @vue-flow 不在首屏
  await page.goto(`${BASE}/dashboard`, { waitUntil: 'domcontentloaded' });
  await page.waitForTimeout(1500);
  const firstScreen = await page.evaluate(() => ({
    scripts: performance
      .getEntriesByType('resource')
      .filter((e) => /\.(js|mjs|ts|vue)(\?|$)/.test(e.name))
      .map((e) => ({ name: e.name.split('/').pop(), bytes: e.transferSize || e.encodedBodySize || 0 })),
  }));

  const result = {
    browser: ENGINE,
    baseUrl: BASE,
    defId: DEF,
    rounds: ROUNDS,
    createdAt: new Date().toISOString(),
    vueflow: {
      samples: samples.vueflow.map((s) => s.ms),
      medianMs: median(samples.vueflow.map((s) => s.ms)),
      medianScriptKB: Number((median(samples.vueflow.map((s) => s.scriptBytes)) / 1024).toFixed(1)),
    },
    native: {
      samples: samples.native.map((s) => s.ms),
      medianMs: median(samples.native.map((s) => s.ms)),
      medianScriptKB: Number((median(samples.native.map((s) => s.scriptBytes)) / 1024).toFixed(1)),
    },
    deltaPercent: null,
    firstScreenScripts: firstScreen.scripts,
    firstScreenHasVueFlow: firstScreen.scripts.some((s) => /vue-flow|vueflow|FlowCanvasVueFlow/i.test(s.name || '')),
  };
  result.deltaPercent = Number((((result.vueflow.medianMs - result.native.medianMs) / result.native.medianMs) * 100).toFixed(1));

  fs.mkdirSync(path.dirname(OUT), { recursive: true });
  fs.writeFileSync(OUT, JSON.stringify(result, null, 2));
  process.stdout.write(
    `\n${ENGINE}: vueflow 中位数 ${result.vueflow.medianMs}ms / native ${result.native.medianMs}ms → ${result.deltaPercent > 0 ? '+' : ''}${result.deltaPercent}%\n` +
      `首屏脚本里出现 @vue-flow ？ ${result.firstScreenHasVueFlow}（应为 false）\nJSON: ${OUT}\n`,
  );
  await browser.close();
  if (Math.abs(result.deltaPercent) > 20) process.exitCode = 1;
})().catch((error) => {
  process.stdout.write(`FATAL: ${error && error.stack ? error.stack : error}\n`);
  process.exitCode = 1;
});
