// 一次性静态服务：dist 命中即返回，未命中回落 index.html（SPA），/api 反代到本地后端
import http, { createServer } from 'node:http';
import { readFile, stat } from 'node:fs/promises';
import { join, extname, normalize } from 'node:path';

const [root, portArg, upstreamArg] = process.argv.slice(2);
const PORT = Number(portArg || 5301);
const [UP_HOST, UP_PORT] = (upstreamArg || '127.0.0.1:18023').split(':');

const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'application/javascript; charset=utf-8',
  '.mjs': 'application/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.jpg': 'image/jpeg',
  '.ico': 'image/x-icon',
  '.woff2': 'font/woff2',
  '.woff': 'font/woff',
  '.ttf': 'font/ttf',
  '.map': 'application/json',
};

async function sendFile(res, path) {
  const buf = await readFile(path);
  res.writeHead(200, { 'content-type': MIME[extname(path)] || 'application/octet-stream' });
  res.end(buf);
}

createServer(async (req, res) => {
  const url = new URL(req.url, 'http://x');
  if (url.pathname.startsWith('/api') || url.pathname === '/health') {
    const up = http.request(
      { host: UP_HOST, port: UP_PORT, path: req.url, method: req.method, headers: { ...req.headers, host: `${UP_HOST}:${UP_PORT}` } },
      (ur) => {
        res.writeHead(ur.statusCode || 502, ur.headers);
        ur.pipe(res);
      },
    );
    up.on('error', (e) => {
      res.writeHead(502, { 'content-type': 'text/plain' });
      res.end(`upstream error: ${e.message}`);
    });
    req.pipe(up);
    return;
  }
  const rel = normalize(url.pathname).replace(/^(\.\.(\/|\\|$))+/, '');
  let file = join(root, rel);
  try {
    const st = await stat(file);
    if (st.isDirectory()) file = join(file, 'index.html');
    await sendFile(res, file);
    return;
  } catch {
    // SPA 回落
    try {
      await sendFile(res, join(root, 'index.html'));
    } catch {
      res.writeHead(404);
      res.end('not found');
    }
  }
}).listen(PORT, '127.0.0.1', () => console.log(`serve ${root} on http://127.0.0.1:${PORT} -> ${UP_HOST}:${UP_PORT}`));
