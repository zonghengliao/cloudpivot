// 一次性静态服务 + /api 反代（P0-FIX-4B 真机验收用；仓库外，不提交）
// 静态命中即返回；未命中回落 index.html（SPA）；/api、/health 用 http.request(...).pipe(res) 转发。
import http, { createServer } from 'node:http';
import { readFile, stat } from 'node:fs/promises';
import { join, extname, normalize } from 'node:path';

const ROOT = process.argv[2];
const PORT = Number(process.argv[3] ?? 5287);
const UPSTREAM = process.argv[4] ?? '127.0.0.1:18087';

const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'application/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.jpg': 'image/jpeg',
  '.woff': 'font/woff',
  '.woff2': 'font/woff2',
  '.ttf': 'font/ttf',
  '.ico': 'image/x-icon',
  '.map': 'application/json; charset=utf-8',
};

function proxy(req, res) {
  const [host, port] = UPSTREAM.split(':');
  const upstream = http.request(
    { host, port: Number(port), path: req.url, method: req.method, headers: { ...req.headers, host: UPSTREAM } },
    (up) => {
      res.writeHead(up.statusCode ?? 502, up.headers);
      up.pipe(res);
    },
  );
  upstream.on('error', (error) => {
    res.writeHead(502, { 'content-type': 'application/json' });
    res.end(JSON.stringify({ code: 502, msg: `upstream error: ${error.message}` }));
  });
  req.pipe(upstream);
}

createServer(async (req, res) => {
  if (req.url.startsWith('/api') || req.url.startsWith('/health')) {
    proxy(req, res);
    return;
  }
  const clean = normalize(decodeURIComponent(req.url.split('?')[0])).replace(/^(\.\.[/\\])+/, '');
  let file = join(ROOT, clean);
  try {
    const info = await stat(file);
    if (info.isDirectory()) file = join(file, 'index.html');
  } catch {
    file = join(ROOT, 'index.html');
  }
  try {
    const body = await readFile(file);
    res.writeHead(200, { 'content-type': MIME[extname(file)] ?? 'application/octet-stream', 'cache-control': 'no-store' });
    res.end(body);
  } catch {
    const body = await readFile(join(ROOT, 'index.html'));
    res.writeHead(200, { 'content-type': MIME['.html'] });
    res.end(body);
  }
}).listen(PORT, '127.0.0.1', () => console.log(`serve-dist on ${PORT} -> ${ROOT} / ${UPSTREAM}`));
