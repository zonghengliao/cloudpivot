/**
 * A5 真机验收用的静态服务 + `/api` 反代（纯 node stdlib，60 行）。
 *
 * - 静态命中即返回；未命中回落 `index.html`（SPA 路由）；
 * - `/api`、`/health` 整段转发到隔离后端 `127.0.0.1:18085`；
 * - 转发用 `http.request` + `upstream.pipe(res)`（**不要** `Readable.fromWeb`，
 *   那是 fetch Response 专用，用在 IncomingMessage 上会在第一个 API 请求就抛
 *   ERR_INVALID_ARG_TYPE 把 server 打死）。
 *
 * 用法：`node serve.mjs <dist-dir> <listen-port> <backend-port>`
 */
import http, { createServer } from 'node:http';
import { createReadStream, statSync } from 'node:fs';
import { extname, join, normalize } from 'node:path';

const [distDir, listenPort, backendPort] = process.argv.slice(2);
const MIME = {
  '.html': 'text/html; charset=utf-8',
  '.js': 'text/javascript; charset=utf-8',
  '.css': 'text/css; charset=utf-8',
  '.json': 'application/json; charset=utf-8',
  '.svg': 'image/svg+xml',
  '.png': 'image/png',
  '.ico': 'image/x-icon',
  '.woff2': 'font/woff2',
  '.woff': 'font/woff',
};

function proxy(req, res) {
  const upstream = http.request(
    {
      host: '127.0.0.1',
      port: Number(backendPort),
      path: req.url,
      method: req.method,
      headers: { ...req.headers, host: `127.0.0.1:${backendPort}` },
    },
    (upstreamRes) => {
      res.writeHead(upstreamRes.statusCode ?? 502, upstreamRes.headers);
      upstreamRes.pipe(res);
    },
  );
  upstream.on('error', (error) => {
    res.writeHead(502, { 'content-type': 'text/plain' });
    res.end(`proxy error: ${error.message}`);
  });
  req.pipe(upstream);
}

createServer((req, res) => {
  const url = req.url ?? '/';
  if (url.startsWith('/api') || url.startsWith('/health') || url.startsWith('/metrics')) {
    proxy(req, res);
    return;
  }
  const path = normalize(join(distDir, decodeURIComponent(url.split('?')[0])));
  const file = path.startsWith(distDir) && (() => {
    try {
      return statSync(path).isFile();
    } catch {
      return false;
    }
  })()
    ? path
    : join(distDir, 'index.html');
  res.writeHead(200, { 'content-type': MIME[extname(file)] ?? 'application/octet-stream' });
  createReadStream(file).pipe(res);
}).listen(Number(listenPort), '127.0.0.1', () => {
  console.log(`serve ${distDir} on http://127.0.0.1:${listenPort} -> backend ${backendPort}`);
});
