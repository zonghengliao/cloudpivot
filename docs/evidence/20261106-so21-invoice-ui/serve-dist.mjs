// SO-21 验收用静态服务：dist 静态 + /api|/health 反代到隔离后端（18123）。
// 用法：node serve-dist.mjs <distDir> <port> <backendPort>
import http, { createServer } from 'node:http';
import { createReadStream, existsSync, statSync } from 'node:fs';
import { extname, join, normalize, resolve } from 'node:path';

const DIST = resolve(process.argv[2] ?? '/opt/cloudpivot/frontend/dist');
const PORT = Number(process.argv[3] ?? 5299);
const UPSTREAM = Number(process.argv[4] ?? 18123);

const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.jpg': 'image/jpeg',
  '.ico': 'image/x-icon',
  '.woff': 'font/woff',
  '.woff2': 'font/woff2',
  '.ttf': 'font/ttf',
  '.map': 'application/json; charset=utf-8',
};

function proxy(req, res) {
  const upstream = http.request(
    {
      host: '127.0.0.1',
      port: UPSTREAM,
      method: req.method,
      path: req.url,
      headers: { ...req.headers, host: `127.0.0.1:${UPSTREAM}` },
    },
    (up) => {
      res.writeHead(up.statusCode ?? 502, up.headers);
      up.pipe(res);
    },
  );
  upstream.on('error', (error) => {
    res.writeHead(502, { 'content-type': 'text/plain' });
    res.end(`proxy error: ${error.message}`);
  });
  req.pipe(upstream);
}

const server = createServer((req, res) => {
  const url = new URL(req.url ?? '/', 'http://127.0.0.1');
  if (url.pathname.startsWith('/api') || url.pathname === '/health') {
    proxy(req, res);
    return;
  }
  const rel = normalize(decodeURIComponent(url.pathname)).replace(/^(\.\.[/\\])+/, '');
  let file = join(DIST, rel);
  if (!existsSync(file) || statSync(file).isDirectory()) {
    file = join(DIST, 'index.html');
  }
  res.writeHead(200, { 'content-type': MIME[extname(file)] ?? 'application/octet-stream' });
  createReadStream(file).pipe(res);
});

server.listen(PORT, '127.0.0.1', () => {
  console.log(`static ${DIST} on http://127.0.0.1:${PORT} -> api :${UPSTREAM}`);
});
