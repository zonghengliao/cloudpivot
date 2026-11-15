// B2 像素比对（before / after 逐页）—— 不引入新依赖：用 Chromium 的 canvas 解码 + 逐像素比
//
// 用法（必须在 frontend/ 下跑，`@playwright/test` 才可解析）：
//   node ../docs/evidence/2026-11-16-shell-b3/pixels.mjs --before /tmp/b3/shots/before --after /tmp/b3/shots/after --out /tmp/b3/shots/diff
//   # 对照组：--after 指到同一批（同 dist 连拍两次），差异必须为 0
//
// 判定口径：
//   · 逐像素比 RGBA，任一通道差 > 0 即记 1 个差异像素（截图都是同机同字体，理论上应完全一致）；
//   · 除差异像素数外，还输出**差异包围盒**与「行区间」，用来核对差异是否落在该页「有意变化表」说的位置；
//   · 产出可视 diff 图：差异像素涂红、其余压暗，便于人眼复核。
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';

const req = createRequire(path.join(process.cwd(), '__resolve__.js'));
const chromium = req('@playwright/test').chromium;

const argv = process.argv.slice(2);
const arg = (k, d = null) => { const i = argv.indexOf('--' + k); return i >= 0 ? argv[i + 1] : d; };
const BEFORE = path.resolve(arg('before'));
const AFTER = path.resolve(arg('after'));
const OUT = path.resolve(arg('out'));
fs.mkdirSync(OUT, { recursive: true });

// 静态服务：根目录 = shots 根（before/after 都是它的子目录）
const ROOT = path.commonPrefix ? path.commonPrefix([BEFORE, AFTER]) : path.dirname(BEFORE);
const server = http.createServer((rq, rs) => {
  const p = decodeURIComponent(new URL(rq.url, 'http://x').pathname);
  const f = path.join(ROOT, p);
  if (!f.startsWith(ROOT) || !fs.existsSync(f)) { rs.writeHead(404); rs.end(); return; }
  rs.writeHead(200, { 'content-type': 'image/png' });
  fs.createReadStream(f).pipe(rs);
});
await new Promise((r) => server.listen(5203, '127.0.0.1', r));
const rel = (p) => p.slice(ROOT.length).replace(/\\/g, '/');

const browser = await chromium.launch();
const page = await browser.newPage();
await page.goto('http://127.0.0.1:5203/__blank__').catch(() => {});
await page.setContent('<html><body></body></html>');

const files = fs.readdirSync(BEFORE).filter((f) => f.endsWith('.png')).sort();
const rows = [];
let totalDiff = 0;
for (const f of files) {
  if (!fs.existsSync(path.join(AFTER, f))) { rows.push({ f, err: 'after 缺文件' }); totalDiff++; continue; }
  const res = await page.evaluate(async ([a, b, mask]) => {
    const load = (src) => new Promise((ok, bad) => { const i = new Image(); i.onload = () => ok(i); i.onerror = bad; i.src = src; });
    const ia = await load(a); const ib = await load(b);
    if (ia.width !== ib.width || ia.height !== ib.height) return { sizeMismatch: [ia.width, ia.height, ib.width, ib.height] };
    const draw = (img) => { const c = document.createElement('canvas'); c.width = img.width; c.height = img.height; const x = c.getContext('2d'); x.drawImage(img, 0, 0); return x.getImageData(0, 0, img.width, img.height); };
    const A = draw(ia); const B = draw(ib);
    const diff = document.createElement('canvas'); diff.width = ia.width; diff.height = ia.height;
    const dctx = diff.getContext('2d'); const out = dctx.createImageData(ia.width, ia.height);
    let n = 0, maxD = 0, minX = 1e9, minY = 1e9, maxX = -1, maxY = -1;
    let sig = 0; // 「有意义」的差异：落在 mask 之外且单通道差 > 8（≤8 是抗锯齿/子像素噪声）
    const rowSet = new Set();
    for (let p = 0; p < A.data.length; p += 4) {
      const d = Math.max(Math.abs(A.data[p] - B.data[p]), Math.abs(A.data[p + 1] - B.data[p + 1]), Math.abs(A.data[p + 2] - B.data[p + 2]), Math.abs(A.data[p + 3] - B.data[p + 3]));
      const idx = p / 4; const x = idx % ia.width; const y = (idx - x) / ia.width;
      if (d > 0) {
        n++; if (d > maxD) maxD = d;
        const masked = mask.some(([x0, y0, x1, y1]) => x >= x0 && x <= x1 && y >= y0 && y <= y1);
        if (!masked && d > 8) { sig++; }
        if (x < minX) minX = x; if (x > maxX) maxX = x;
        if (y < minY) minY = y; if (y > maxY) maxY = y;
        rowSet.add(y);
        out.data[p] = 255; out.data[p + 1] = 0; out.data[p + 2] = 0; out.data[p + 3] = 255;
      } else {
        const g = 235;
        out.data[p] = g; out.data[p + 1] = g; out.data[p + 2] = g; out.data[p + 3] = 255;
      }
    }
    dctx.putImageData(out, 0, 0);
    // 把有差异的行压成区间，便于对账
    const ys = [...rowSet].sort((x, y) => x - y);
    const bands = []; for (const y of ys) { const last = bands[bands.length - 1]; if (last && y <= last[1] + 1) last[1] = y; else bands.push([y, y]); }
    return { w: ia.width, h: ia.height, n, sig, maxD, bbox: n ? [minX, minY, maxX, maxY] : null, bands: bands.slice(0, 12), png: diff.toDataURL('image/png') };
  }, ['http://127.0.0.1:5203' + rel(path.join(BEFORE, f)), 'http://127.0.0.1:5203' + rel(path.join(AFTER, f)), [[0, 0, 64, 64]]]);
  if (res.sizeMismatch) { rows.push({ f, err: '尺寸不同 ' + res.sizeMismatch.join('x') }); totalDiff++; continue; }
  fs.writeFileSync(path.join(OUT, f.replace(/\.png$/, '.diff.png')), Buffer.from(res.png.split(',')[1], 'base64'));
  rows.push({ f, n: res.n, sig: res.sig, maxD: res.maxD, bbox: res.bbox, bands: res.bands });
  if (res.n > 0) totalDiff++;
}
for (const r of rows) {
  if (r.err) { console.log(`${r.f}  ✗ ${r.err}`); continue; }
  console.log(`${r.f}  差异像素 ${r.n}（mask 内）  有意义 ${r.sig}${r.n ? `  maxΔ=${r.maxD}  bbox=${JSON.stringify(r.bbox)}  y 区间=${JSON.stringify(r.bands)}` : ''}`);
}
console.log(`\n共 ${rows.length} 页；有差异的页 ${totalDiff} 页；diff 图目录 ${OUT}`);
await browser.close();
server.close();
process.exit(0);
