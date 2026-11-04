#!/usr/bin/env node
/**
 * 极简静态服务 + API 反代（VUE-FLOW-3 性能测量的「生产产物」环境）。
 *
 * 为什么需要它：
 *   · `vite dev` 是按需编译的，同一次 goto 的耗时抖动可达 ±30%（实测同一份代码
 *     中位数在 -29% ~ +24% 之间跳），拿它判「±20%」不可信；
 *   · 生产是 `dist/` 静态资源 + nginx 反代 `/api`，所以正确的量法是：把 `vite build`
 *     的产物原样服务起来，`/api` 指到后端 —— 这时测到的就是用户真实加载的那几个 chunk。
 *
 * 用法：
 *   node scripts/verify/vueflow3_static_preview.cjs --port 5197 --root frontend/dist \
 *     --backend http://127.0.0.1:18150
 *
 * 只做三件事：静态文件（带 SPA fallback 到 index.html）、`/api` 与 `/health` 反代。
 */
'use strict';

const fs = require('node:fs');
const http = require('node:http');
const path = require('node:path');

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
const PORT = Number(args.port || 5197);
const ROOT = path.resolve(args.root || path.join(__dirname, '..', '..', 'frontend', 'dist'));
const BACKEND = new URL(args.backend || 'http://127.0.0.1:18150');

const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.mjs': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.jpg': 'image/jpeg',
  '.ico': 'image/x-icon',
  '.woff': 'font/woff',
  '.woff2': 'font/woff2',
  '.map': 'application/json; charset=utf-8',
};

function proxy(req, res) {
  const upstream = http.request(
    {
      hostname: BACKEND.hostname,
      port: BACKEND.port,
      path: req.url,
      method: req.method,
      headers: { ...req.headers, host: BACKEND.host },
    },
    (up) => {
      res.writeHead(up.statusCode || 502, up.headers);
      up.pipe(res);
    },
  );
  upstream.on('error', (error) => {
    res.writeHead(502, { 'Content-Type': 'application/json' });
    res.end(JSON.stringify({ code: 502, msg: `proxy error: ${error.message}` }));
  });
  req.pipe(upstream);
}

const server = http.createServer((req, res) => {
  const url = new URL(req.url, `http://127.0.0.1:${PORT}`);
  if (url.pathname.startsWith('/api') || url.pathname === '/health') {
    proxy(req, res);
    return;
  }
  // 静态资源：命中文件就发文件，否则 SPA fallback 到 index.html
  let file = path.join(ROOT, decodeURIComponent(url.pathname));
  if (!file.startsWith(ROOT)) {
    res.writeHead(403).end('forbidden');
    return;
  }
  if (url.pathname.endsWith('/')) file = path.join(file, 'index.html');
  fs.stat(file, (error, stat) => {
    if (error || !stat.isFile()) {
      if (/\.[a-z0-9]+$/i.test(url.pathname)) {
        res.writeHead(404).end('not found');
        return;
      }
      file = path.join(ROOT, 'index.html');
      fs.readFile(file, (err, buf) => {
        if (err) {
          res.writeHead(404).end('not found');
          return;
        }
        res.writeHead(200, { 'Content-Type': MIME['.html'] });
        res.end(buf);
      });
      return;
    }
    const ext = path.extname(file).toLowerCase();
    res.writeHead(200, {
      'Content-Type': MIME[ext] || 'application/octet-stream',
      'Content-Length': stat.size,
      'Cache-Control': 'no-cache',
    });
    fs.createReadStream(file).pipe(res);
  });
});

server.listen(PORT, '127.0.0.1', () => {
  process.stdout.write(`static preview: http://127.0.0.1:${PORT} → ${ROOT} (api → ${BACKEND.origin})\n`);
});
