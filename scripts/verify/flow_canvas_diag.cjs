#!/usr/bin/env node
/**
 * 流程画布节点定位诊断（Chromium / WebKit 双引擎对比）。
 *
 * # 为什么有这个脚本
 *
 * Safari 下曾出现「节点全部塌陷到画布左上角」（连线 / 条件标签 / 缩略图都正常，只有 foreignObject
 * 卡片跑位）。这类问题**不能只看 getBoundingClientRect**：
 *
 *   2026-11 实测（Playwright WebKit 2370 / Safari 27.2 UA，改前 `<g transform>` + foreignObject）：
 *     · getBoundingClientRect 全部正确（卡片 623,516 / 789,410 … 与 Chrome 一致）
 *     · 但截图上 6 张卡片全部挤在画布左上角叠成一堆
 *   → WebKit 这是**绘制层**的 bug：布局对、画出来错。所以本脚本除了量几何，还要验「卡片像素
 *     是否真的画在它的矩形里」（paint check：隐藏 foreignObject 前后同一块区域有没有变化）。
 *
 * # 输出
 *
 *   · 每个节点的：`<g>` 上的 transform、foreignObject 的 x/y/w/h、卡片 computed 尺寸、
 *     卡片 / foreignObject / `<g>` 的 getBoundingClientRect、锚点与校验角标的位置
 *   · 每个节点的 paint 结果（卡片矩形内「隐藏前 vs 隐藏后」的差异像素数）
 *   · 判定：ok / collapsed（几何重叠）/ not-painted（几何正常但没画在位置上）/ no-nodes
 *
 * # 用法
 *
 *   node scripts/verify/flow_canvas_diag.cjs --browser chromium --token-file /tmp/t.txt \
 *        --base-url http://127.0.0.1:5187 --def-id <流程定义id>
 *
 *   --browser   chromium | webkit（Playwright 的 WebKit ≈ Safari 内核；真机 Safari 仍需人工确认）
 *   --base-url  前端地址（默认 http://127.0.0.1:5173）
 *   --def-id    流程定义 id（路由是 /flow/def/:id/edit，注意**没有** /business 前缀）
 *   --token-file / --token  登录后的 access token（写进 localStorage.x_access_token）
 *   --out       完整 JSON 落盘路径（默认 <cwd>/flow_canvas_diag_<browser>.json）
 *   --shot      截图路径（默认 <cwd>/flow_canvas_diag_<browser>.png）
 *   --wait      页面就绪后的额外等待毫秒（默认 1200，等 fitView / 过渡动画）
 *   --no-paint  跳过 paint check（只量几何，快一点）
 *
 * 退出码：0 = 有节点、几何与绘制都正常；1 = 异常（collapsed / not-painted）；2 = 页面没画出节点。
 */
'use strict';

const fs = require('node:fs');
const path = require('node:path');

/** 从脚本目录向上找 Playwright（本仓 frontend/node_modules 里有一份） */
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
  throw new Error('找不到 Playwright（试过 @playwright/test 与 frontend/node_modules）；请在仓库内运行');
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
    } else {
      const next = argv[i + 1];
      if (next && !next.startsWith('--')) {
        out[token.slice(2)] = next;
        i += 1;
      } else {
        out[token.slice(2)] = 'true';
      }
    }
  }
  return out;
}

const args = parseArgs(process.argv.slice(2));
const browserName = (args.browser || 'chromium').toLowerCase();
const baseUrl = (args['base-url'] || 'http://127.0.0.1:5173').replace(/\/$/, '');
const defId = args['def-id'] || '';
const waitMs = Number(args.wait || 1200);
const outPath = args.out || path.join(process.cwd(), `flow_canvas_diag_${browserName}.json`);
const shotPath = args.shot || path.join(process.cwd(), `flow_canvas_diag_${browserName}.png`);
const withPaint = args['no-paint'] !== 'true';

let token = args.token || '';
if (!token && args['token-file']) {
  token = fs.readFileSync(args['token-file'], 'utf8').trim();
}
if (!defId) {
  console.error('缺少 --def-id（流程定义 id）');
  process.exit(64);
}

/** 页面内探针：量每个节点的 transform / foreignObject 属性 / 卡片 computed / 实际 rect */
const probe = () => {
  const round = (value) => Math.round(value * 100) / 100;
  const svg = document.querySelector('.flow-canvas__svg');
  const scene = document.querySelector('.flow-canvas__svg > g');
  const wrap = document.querySelector('.flow-canvas');
  // 平移 / 缩放有两种承载方式：老代码是 scene <g> 的 transform 属性，新代码是 svg 元素的 CSS transform。
  // 两种都读出来，脚本对改法不敏感。
  const cssMatrix = svg ? getComputedStyle(svg).transform : 'none';
  const numbers = cssMatrix && cssMatrix.startsWith('matrix(') ? cssMatrix.slice(7, -1).split(',').map(Number) : null;
  const view = numbers
    ? { scale: Math.round(numbers[0] * 10000) / 10000, x: Math.round(numbers[4] * 100) / 100, y: Math.round(numbers[5] * 100) / 100 }
    : null;
  const nodes = [...document.querySelectorAll('.flow-node')].map((group) => {
    const fo = group.querySelector('foreignObject');
    const card = group.querySelector('.flow-node__card');
    const cardRect = card.getBoundingClientRect();
    const foRect = fo.getBoundingClientRect();
    const groupRect = group.getBoundingClientRect();
    const style = getComputedStyle(card);
    const ports = [...group.querySelectorAll('.flow-node__port')].map((circle) => {
      const rect = circle.getBoundingClientRect();
      return {
        cx: circle.getAttribute('cx'),
        cy: circle.getAttribute('cy'),
        left: round(rect.left),
        top: round(rect.top),
        width: round(rect.width),
      };
    });
    const issue = group.querySelector('.flow-node__issue');
    return {
      key: group.getAttribute('data-node-key'),
      type: group.getAttribute('data-node-type'),
      groupTransform: group.getAttribute('transform'),
      foAttr: {
        x: fo.getAttribute('x'),
        y: fo.getAttribute('y'),
        width: fo.getAttribute('width'),
        height: fo.getAttribute('height'),
      },
      cardComputed: { width: style.width, height: style.height },
      cardRect: {
        left: round(cardRect.left),
        top: round(cardRect.top),
        width: round(cardRect.width),
        height: round(cardRect.height),
      },
      foRect: {
        left: round(foRect.left),
        top: round(foRect.top),
        width: round(foRect.width),
        height: round(foRect.height),
      },
      groupRect: {
        left: round(groupRect.left),
        top: round(groupRect.top),
        width: round(groupRect.width),
        height: round(groupRect.height),
      },
      ports,
      issue: issue
        ? {
            transform: issue.getAttribute('transform'),
            ...(function () {
              const rect = issue.getBoundingClientRect();
              return { left: round(rect.left), top: round(rect.top) };
            })(),
          }
        : null,
    };
  });
  return {
    userAgent: navigator.userAgent,
    hasSvg: Boolean(svg),
    /** `<g>` 上的 transform 属性（老改法；新改法应为 null） */
    sceneTransform: scene ? scene.getAttribute('transform') : null,
    /** svg 元素的计算 transform（新改法；形如 matrix(s,0,0,s,tx,ty)） */
    sceneCssTransform: cssMatrix,
    /** 由上面的 matrix 解出的平移 / 缩放 */
    view,
    canvasRect: wrap
      ? (function () {
          const rect = wrap.getBoundingClientRect();
          return { left: round(rect.left), top: round(rect.top), width: round(rect.width), height: round(rect.height) };
        })()
      : null,
    nodeCount: nodes.length,
    nodes,
  };
};

(async () => {
  const playwright = loadPlaywright();
  const engine = playwright[browserName];
  if (!engine) {
    console.error(`不支持的浏览器：${browserName}（只支持 chromium / webkit）`);
    process.exit(64);
  }

  const browser = await engine.launch();
  const context = await browser.newContext({ viewport: { width: 1600, height: 1000 }, deviceScaleFactor: 1 });
  // 令牌必须在页面脚本执行前就位（路由守卫读 localStorage）
  await context.addInitScript((value) => {
    if (value) {
      localStorage.setItem('x_access_token', value);
      localStorage.setItem('cp-onboarding-done', '1');
    }
    // 登录后「你有 N 条待处理」汇总弹窗（NOTIFY-POP）是异步拉 4 个 count 才渲染的，
    // 会让截图在同一份代码上时有时无（实测同一张图两次跑差 6000 像素全是它）。
    // 诊断画布几何时把它按掉，截图才可比。
    sessionStorage.setItem('login-unread-shown', '1');
  }, token);

  const page = await context.newPage();
  const consoleErrors = [];
  page.on('console', (message) => {
    if (message.type() === 'error') consoleErrors.push(message.text());
  });
  page.on('pageerror', (error) => consoleErrors.push(`pageerror: ${error.message}`));

  const url = `${baseUrl}/flow/def/${defId}/edit`;
  await page.goto(url, { waitUntil: 'domcontentloaded' });
  // 画布挂载 → 取数 → fitView（rAF）→ 过渡；给足时间再量
  await page.waitForSelector('.flow-node', { timeout: 30000 }).catch(() => undefined);
  await page.waitForTimeout(waitMs);

  const data = await page.evaluate(probe);
  data.url = url;
  data.browser = browserName;
  data.consoleErrors = consoleErrors;

  await page.screenshot({ path: shotPath, fullPage: false });

  // ---- 绘制自检：把 foreignObject 从 DOM 里删掉前后，同一块卡片矩形内的像素变化 ----
  // 布局对但画错位置时（WebKit 的已知形态），矩形内「删前 vs 删后」几乎无差异 → 说明卡片没画在这儿。
  // 注意：不能用 `visibility: hidden` / `opacity: 0` —— WebKit 对 foreignObject 上的这两个属性
  // 不生效（实测像素变化 0），只有真删掉节点才能看到差异。
  if (withPaint && data.nodeCount > 0) {
    const before = (await page.screenshot({ fullPage: false })).toString('base64');
    await page.evaluate(() => {
      document.querySelectorAll('foreignObject').forEach((fo) => fo.remove());
    });
    await page.waitForTimeout(250);
    const after = (await page.screenshot({ fullPage: false })).toString('base64');

    // 解码 + 比较放到一个空白页里做（避免依赖图像库）
    const helper = await context.newPage();
    await helper.goto('about:blank');
    const perNode = await helper.evaluate(
      async ([beforeUrl, afterUrl, rects]) => {
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
        const a = await pixels(beforeUrl);
        const b = await pixels(afterUrl);
        return rects.map((rect) => {
          const x1 = Math.max(0, Math.floor(rect.left));
          const y1 = Math.max(0, Math.floor(rect.top));
          const x2 = Math.min(a.width, Math.ceil(rect.left + rect.width));
          const y2 = Math.min(a.height, Math.ceil(rect.top + rect.height));
          let changed = 0;
          let total = 0;
          for (let y = y1; y < y2; y += 1) {
            for (let x = x1; x < x2; x += 1) {
              const i = (y * a.width + x) * 4;
              const delta = Math.max(
                Math.abs(a.data[i] - b.data[i]),
                Math.abs(a.data[i + 1] - b.data[i + 1]),
                Math.abs(a.data[i + 2] - b.data[i + 2]),
              );
              if (delta > 8) changed += 1;
              total += 1;
            }
          }
          return { key: rect.key, changed, total, ratio: total ? changed / total : 0 };
        });
      },
      [
        `data:image/png;base64,${before}`,
        `data:image/png;base64,${after}`,
        data.nodes.map((node) => ({ key: node.key, ...node.cardRect })),
      ],
    );
    await helper.close();

    const byKey = new Map(perNode.map((item) => [item.key, item]));
    for (const node of data.nodes) {
      const paint = byKey.get(node.key);
      node.paint = paint ? { changed: paint.changed, total: paint.total, ratio: Math.round(paint.ratio * 1000) / 1000 } : null;
    }
  }

  fs.writeFileSync(outPath, JSON.stringify(data, null, 2));

  console.log(`\n===== ${browserName} =====`);
  console.log(`url            ${url}`);
  console.log(`ua             ${data.userAgent}`);
  console.log(`平移缩放承载     ${data.sceneTransform ? `scene <g transform>=${data.sceneTransform}` : `svg 的 CSS transform=${data.sceneCssTransform}`}`);
  if (data.view) console.log(`view           x=${data.view.x} y=${data.view.y} scale=${data.view.scale}`);
  if (data.canvasRect) console.log(`canvas rect    ${JSON.stringify(data.canvasRect)}`);
  console.log(`节点数          ${data.nodeCount}`);
  console.log('');
  for (const node of data.nodes) {
    console.log(
      `${String(node.key).padEnd(10)} transform=${String(node.groupTransform).padEnd(30)} ` +
        `fo(x,y,w,h)=(${node.foAttr.x},${node.foAttr.y},${node.foAttr.width},${node.foAttr.height}) ` +
        `card computed=${node.cardComputed.width}x${node.cardComputed.height} ` +
        `card rect=(${node.cardRect.left},${node.cardRect.top}) ${node.cardRect.width}x${node.cardRect.height} ` +
        `fo rect=(${node.foRect.left},${node.foRect.top})` +
        (node.paint ? ` paint=${node.paint.changed}/${node.paint.total} (${Math.round(node.paint.ratio * 100)}%)` : ''),
    );
    if (node.ports.length) console.log(`           ports=${JSON.stringify(node.ports)}`);
    if (node.issue) console.log(`           issue=${JSON.stringify(node.issue)}`);
  }

  // ---- 判定 ----
  let verdict = 'ok';
  if (data.nodeCount === 0) {
    verdict = 'no-nodes';
  } else {
    const lefts = data.nodes.map((node) => node.cardRect.left);
    const tops = data.nodes.map((node) => node.cardRect.top);
    const spanX = Math.max(...lefts) - Math.min(...lefts);
    const spanY = Math.max(...tops) - Math.min(...tops);
    const maxW = Math.max(...data.nodes.map((node) => node.cardRect.width));
    const maxH = Math.max(...data.nodes.map((node) => node.cardRect.height));
    data.spread = { x: Math.round(spanX * 100) / 100, y: Math.round(spanY * 100) / 100, maxW, maxH };
    console.log(`\n卡片实际跨度    x=${data.spread.x}  y=${data.spread.y}（最大卡片 ${maxW}x${maxH}）`);
    if (data.nodeCount > 1 && spanX < maxW * 0.5 && spanY < maxH * 0.5) {
      verdict = 'collapsed';
    }
    // 绘制自检：卡片矩形内「隐藏前后」至少要有 2% 像素变化（卡片本身 + 边框 + 阴影）
    if (verdict === 'ok' && withPaint) {
      const unpainted = data.nodes.filter((node) => node.paint && node.paint.ratio < 0.02);
      if (unpainted.length > 0) {
        verdict = 'not-painted';
        data.unpainted = unpainted.map((node) => node.key);
        console.log(
          `\n⚠ 这些节点在布局位置上**没有画出卡片**（隐藏 foreignObject 前后像素几乎不变）：` +
            `${data.unpainted.join(', ')} —— 典型形态是 WebKit 把 foreignObject 画到了画布原点`,
        );
      }
    }
  }
  data.verdict = verdict;
  fs.writeFileSync(outPath, JSON.stringify(data, null, 2));
  console.log(`\n结论            ${verdict}${verdict === 'collapsed' ? '（所有卡片几乎同点 → 塌陷）' : ''}${verdict === 'not-painted' ? '（几何正常但没画在位置上 → 绘制层塌陷）' : ''}`);
  console.log(`JSON            ${outPath}`);
  console.log(`截图            ${shotPath}`);
  if (consoleErrors.length) {
    console.log(`控制台错误       ${consoleErrors.length} 条`);
    for (const text of consoleErrors.slice(0, 5)) console.log(`  · ${text}`);
  }

  await browser.close();
  process.exit(verdict === 'ok' ? 0 : verdict === 'no-nodes' ? 2 : 1);
})().catch((error) => {
  console.error(error);
  process.exit(70);
});
