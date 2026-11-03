/**
 * FAVICON 图标集：线上真机证据（Chromium + WebKit 双引擎）。
 *
 *   cd frontend && node ../scripts/verify/favicon_evidence.cjs
 *
 * 产物写进 docs/evidence/20261103-favicon/：
 *   01-chromium-prod.png        线上首页（新开 context）
 *   02-chromium-prod-incognito.png  线上首页（无痕等价：全新 browser context，无任何已有缓存）
 *   03-webkit-prod.png          线上首页（WebKit 引擎 = Safari 同源内核）
 *   04-icon-set-rendered.png    四个图标按真实尺寸渲染（证明字节可解码、是同一朵云涡）
 *   favicon-network.txt         网络层实录：每个图标请求的 status / content-type / 字节数
 *   favicon-links.txt           页面实际解析出的 <link rel=icon*> 列表
 *
 * 说明：headless 浏览器没有可见的标签栏/书签栏，截不到「标签页上的小图标」，
 * 所以这里用「网络层拿到什么 + 浏览器解码成什么尺寸」两条硬证据代替；
 * 真正肉眼要看的标签页/书签，由人用本机 Safari/Chrome 打开线上站点确认。
 */
const path = require('path');
const fs = require('fs');

const PW_CANDIDATES = [
  process.env.FAVICON_PLAYWRIGHT,
  path.resolve(__dirname, '../../frontend/node_modules/@playwright/test'),
  path.resolve(__dirname, '../../frontend/node_modules/playwright'),
].filter(Boolean);
const PW_PATH = PW_CANDIDATES.find((c) => fs.existsSync(c));
if (!PW_PATH) {
  console.error('找不到 playwright（试过：' + PW_CANDIDATES.join(', ') + '）');
  process.exit(2);
}
const pw = require(PW_PATH);

const BASE = process.env.FAVICON_BASE || 'http://172.18.10.144';
const OUT = process.env.FAVICON_EVIDENCE || '/opt/cloudpivot/docs/evidence/20261103-favicon';
fs.mkdirSync(OUT, { recursive: true });

const ICON_RE = /favicon|apple-touch-icon|mstile|android-chrome|safari-pinned/i;
const netLog = [];
const linkLog = [];
const lines = [];

function hook(page, engine) {
  page.on('response', async (res) => {
    const url = res.url();
    if (!ICON_RE.test(url)) return;
    const req = res.request();
    let len = '';
    try {
      const body = await res.body();
      len = String(body.length);
    } catch (e) {
      len = 'n/a';
    }
    let ct = '';
    try {
      ct = (await res.allHeaders())['content-type'] || '';
    } catch (e) {
      ct = '';
    }
    netLog.push(
      `${engine}\t${res.status()}\t${req.method()}\t${ct}\t${len}B\t${url}`
    );
  });
}

async function dumpLinks(page, engine) {
  const data = await page.evaluate(() => {
    const q = (sel) => [...document.querySelectorAll(sel)];
    return {
      title: document.title,
      href: location.href,
      icons: q('link[rel*=icon]').map((l) => ({
        rel: l.rel,
        sizes: l.sizes ? l.sizes.value : '',
        type: l.type || '',
        href: l.getAttribute('href'),
        abs: l.href,
      })),
      // 页面里旧 favicon.png 的声明必须已经没了
      legacyPng: q('link[rel=icon]').filter((l) => (l.getAttribute('href') || '').includes('/favicon.png')).length,
    };
  });
  linkLog.push(`# ${engine}\n` + JSON.stringify(data, null, 2));
  return data;
}

(async () => {
  const results = [];

  // ---------- 1) Chromium：正常 context ----------
  const chromium = await pw.chromium.launch();
  let ctx = await chromium.newContext({ viewport: { width: 1280, height: 800 } });
  let page = await ctx.newPage();
  hook(page, 'chromium');
  await page.goto(BASE + '/', { waitUntil: 'networkidle' });
  const d1 = await dumpLinks(page, 'chromium');
  await page.screenshot({ path: path.join(OUT, '01-chromium-prod.png') });
  // 让浏览器把四个图标真解码一遍，顺带记录 naturalWidth
  const decoded = await page.evaluate(async () => {
    const names = ['favicon.ico', 'favicon-32x32.png', 'favicon-16x16.png', 'apple-touch-icon.png'];
    const out = {};
    for (const n of names) {
      const im = new Image();
      im.src = '/' + n + '?decode=' + Date.now();
      await im.decode().catch(() => {});
      out[n] = { complete: im.complete, w: im.naturalWidth, h: im.naturalHeight };
    }
    return out;
  });
  await ctx.close();

  // ---------- 2) Chromium：无痕等价（全新 context，不共享缓存）----------
  ctx = await chromium.newContext({ viewport: { width: 1280, height: 800 } });
  page = await ctx.newPage();
  hook(page, 'chromium-incognito');
  await page.goto(BASE + '/', { waitUntil: 'networkidle' });
  await dumpLinks(page, 'chromium-incognito');
  await page.screenshot({ path: path.join(OUT, '02-chromium-prod-incognito.png') });

  // ---------- 3) 图标集渲染：真实尺寸 ----------
  await ctx.newPage().then(async (p) => {
    await p.setContent(`<!doctype html><html><head><meta charset="utf-8">
      <style>
        body{margin:0;padding:24px;font:14px/1.5 -apple-system,"PingFang SC",sans-serif;background:#f5f7fa;color:#1f2d3d}
        h1{font-size:16px;margin:0 0 16px}
        .row{display:flex;gap:28px;align-items:flex-end;flex-wrap:wrap}
        .cell{background:#fff;border:1px solid #e3e6eb;border-radius:8px;padding:12px 14px;text-align:center;min-width:120px}
        .cell img{display:block;margin:0 auto 8px;background:
          repeating-conic-gradient(#e9edf2 0% 25%, #ffffff 0% 50%) 50%/12px 12px}
        .cap{font-size:12px;color:#5c6b7a}
        .name{font-weight:600;margin-bottom:2px}
        .big img{image-rendering:pixelated}
      </style></head><body>
      <h1>favicon 图标集（线上字节，按真实像素尺寸渲染；棋盘格 = 透明通道）</h1>
      <div class="row">
        <div class="cell big"><img src="${BASE}/favicon-16x16.png?v=2" width="16" height="16"><div class="name">favicon-16x16.png</div><div class="cap">16×16 · 普屏标签页</div></div>
        <div class="cell big"><img src="${BASE}/favicon-32x32.png?v=2" width="32" height="32"><div class="name">favicon-32x32.png</div><div class="cap">32×32 · 高分屏标签页</div></div>
        <div class="cell big"><img src="${BASE}/favicon.ico?v=2" width="48" height="48"><div class="name">favicon.ico</div><div class="cap">48×48 帧（含 16/32/48）</div></div>
        <div class="cell"><img src="${BASE}/apple-touch-icon.png?v=2" width="180" height="180" style="width:180px;height:180px"><div class="name">apple-touch-icon.png</div><div class="cap">180×180 · 加书签/主屏</div></div>
      </div></body></html>`);
    await p.waitForTimeout(600);
    await p.screenshot({ path: path.join(OUT, '04-icon-set-rendered.png'), fullPage: true });
  });
  await ctx.close();

  // ---------- 4) WebKit（Safari 同源内核）----------
  let webkitNote = '';
  try {
    const webkit = await pw.webkit.launch();
    const wctx = await webkit.newContext({ viewport: { width: 1280, height: 800 } });
    const wp = await wctx.newPage();
    hook(wp, 'webkit');
    await wp.goto(BASE + '/', { waitUntil: 'load' });
    await wp.waitForTimeout(2500); // 给 WebKit 一点时间去探测图标
    const d3 = await dumpLinks(wp, 'webkit');
    results.push(d3);
    await wp.screenshot({ path: path.join(OUT, '03-webkit-prod.png') });
    await wctx.close();
    await webkit.close();
  } catch (e) {
    webkitNote = 'WebKit 启动失败：' + e.message;
    console.error(webkitNote);
  }

  // ---------- 落盘 ----------
  fs.writeFileSync(path.join(OUT, 'favicon-network.txt'), netLog.join('\n') + '\n');
  fs.writeFileSync(
    path.join(OUT, 'favicon-links.txt'),
    linkLog.join('\n\n') +
      `\n\n# chromium 解码结果\n` +
      JSON.stringify(decoded, null, 2) +
      (webkitNote ? `\n\n# webkit\n${webkitNote}` : '') +
      '\n'
  );

  console.log('links(chromium):', JSON.stringify(d1, null, 2));
  console.log('decoded:', JSON.stringify(decoded, null, 2));
  console.log('--- 图标网络请求 ---');
  console.log(netLog.join('\n') || '(无)');
  if (webkitNote) console.log(webkitNote);
  console.log('证据目录:', OUT);
  await chromium.close();
})().catch((e) => {
  console.error(e);
  process.exit(1);
});
