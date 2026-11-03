// 一次性静态服务 + /api 反代（验证用，仓库外）
import http, { createServer } from 'node:http';
import { createReadStream, existsSync, statSync } from 'node:fs';
import { extname, join, normalize } from 'node:path';

const ROOT = process.argv[2];
const PORT = Number(process.argv[3] || 5290);
const UPSTREAM = process.argv[4] || '127.0.0.1:8023';

const TYPES = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.png': 'image/png',
  '.jpg': 'image/jpeg',
  '.svg': 'image/svg+xml',
  '.ico': 'image/x-icon',
  '.woff2': 'font/woff2',
  '.woff': 'font/woff',
  '.ttf': 'font/ttf',
  '.map': 'application/json',
};

const server = createServer((req, res) => {
  const url = new URL(req.url, 'http://127.0.0.1');
  if (url.pathname.startsWith('/api') || url.pathname === '/health') {
    const upstream = http.request(
      {
        host: UPSTREAM.split(':')[0],
        port: Number(UPSTREAM.split(':')[1]),
        path: req.url,
        method: req.method,
        headers: { ...req.headers, host: UPSTREAM },
      },
      (upstreamRes) => {
        res.writeHead(upstreamRes.statusCode || 502, upstreamRes.headers);
        upstreamRes.pipe(res);
      },
    );
    upstream.on('error', (error) => {
      res.writeHead(502, { 'Content-Type': 'text/plain; charset=utf-8' });
      res.end(`upstream error: ${error.message}`);
    });
    req.pipe(upstream);
    return;
  }

  const rel = normalize(decodeURIComponent(url.pathname)).replace(/^\/+/, '');
  let file = join(ROOT, rel);
  if (!rel || !existsSync(file) || statSync(file).isDirectory()) {
    file = join(ROOT, 'index.html');
  }
  const type = TYPES[extname(file)] || 'application/octet-stream';
  res.writeHead(200, { 'Content-Type': type, 'Cache-Control': 'no-store' });
  createReadStream(file).pipe(res);
});

server.listen(PORT, '127.0.0.1', () => {
  console.log(`serving ${ROOT} on http://127.0.0.1:${PORT} (api -> ${UPSTREAM})`);
});
