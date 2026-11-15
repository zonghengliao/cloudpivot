// B6 探针 diff：按「路由 + CSS 路径」对齐元素，逐属性比对计算样式与几何。
//
// 用法（frontend/ 下）：
//   node ../docs/evidence/2026-11-16-shell-b6/probe-diff-b6.mjs before.json after.json \
//        --allow ../docs/evidence/2026-11-16-shell-b6/probe-allow.json [--only-changed]
//
// 判定：
//   · CLASS-ONLY —— class 属性变了，40+ 计算属性与几何**逐项相同** → 改名组的过档证据（不算差异，单独计数）
//   · STYLE-DIFF / RECT-DIFF —— 计算值/几何变化，必须命中 `--allow`（本批有意变化），否则退出码 1
//   · MISSING / ADDED —— DOM 结构变化（类名替换不该产生），直接算失败
import fs from 'node:fs';
import path from 'node:path';

const argv = process.argv.slice(2);
const files = argv.filter((a) => !a.startsWith('--') && !argv[argv.indexOf(a) - 1]?.startsWith('--allow'));
const A = argv[0];
const B = argv[1];
const ai = argv.indexOf('--allow');
const ALLOW_FILE = ai >= 0 ? argv[ai + 1] : null;
const ONLY_CHANGED = argv.includes('--only-changed');

const before = JSON.parse(fs.readFileSync(A, 'utf8'));
const after = JSON.parse(fs.readFileSync(B, 'utf8'));
const allow = ALLOW_FILE && fs.existsSync(ALLOW_FILE) ? JSON.parse(fs.readFileSync(ALLOW_FILE, 'utf8')) : { rules: [] };

const round = (v) => (typeof v === 'string' ? v.trim() : v);
const key = (route, el) => route + '\u0000' + el.path;

function allowHit(route, el, prop, from, to) {
  for (const r of allow.rules) {
    if (r.route && r.route !== route) continue;
    if (r.path && r.path !== el.path) continue;
    if (r.pathContains && !el.path.includes(r.pathContains)) continue;
    if (r.cls && !new RegExp(r.cls).test(el.cls)) continue;
    if (r.prop && r.prop !== prop) continue;
    if (r.from !== undefined && String(r.from) !== String(from)) continue;
    if (r.to !== undefined && String(r.to) !== String(to)) continue;
    return r;
  }
  return null;
}

let totalEls = 0;
const summary = { routes: 0, classOnly: 0, styleDiff: 0, rectDiff: 0, missing: 0, added: 0, allowed: 0, unallowed: 0 };
const perRoute = [];
const unallowedLines = [];
const allowedByRule = {};

for (const route of Object.keys(before.routes)) {
  const b = before.routes[route];
  const a = after.routes[route];
  if (!a) { unallowedLines.push(`ROUTE-MISSING ${route}`); summary.unallowed++; continue; }
  summary.routes++;
  const bMap = new Map(b.elements.map((e) => [key(route, e), e]));
  const aMap = new Map(a.elements.map((e) => [key(route, e), e]));
  const st = { route, elements: bMap.size, classOnly: 0, styleDiff: 0, rectDiff: 0, missing: 0, added: 0, allowed: 0 };
  for (const [k, be] of bMap) {
    const ae = aMap.get(k);
    if (!ae) { st.missing++; summary.missing++; unallowedLines.push(`MISSING ${route}  ${be.path}  .${be.cls}`); continue; }
    totalEls++;
    const styleDiffs = [];
    for (const p of Object.keys(be.style)) {
      const f = round(be.style[p]);
      const t = round(ae.style[p]);
      if (f === t) continue;
      const hit = allowHit(route, be, p, f, t);
      if (hit) { summary.allowed++; st.allowed++; allowedByRule[hit.why] = (allowedByRule[hit.why] || 0) + 1; }
      else styleDiffs.push({ prop: p, from: f, to: t });
    }
    const rectChanged = ['x', 'y', 'w', 'h'].filter((p) => be.rect[p] !== ae.rect[p]);
    const rectDiffs = rectChanged.map((p) => {
      const hit = allowHit(route, be, 'rect.' + p, be.rect[p], ae.rect[p]);
      if (hit) { summary.allowed++; st.allowed++; allowedByRule[hit.why] = (allowedByRule[hit.why] || 0) + 1; }
      return { prop: 'rect.' + p, from: be.rect[p], to: ae.rect[p], allowed: !!hit };
    });
    const clsChanged = be.cls !== ae.cls;
    if (!styleDiffs.length && !rectDiffs.length) {
      if (clsChanged) { st.classOnly++; summary.classOnly++; }
      continue;
    }
    if (styleDiffs.length) {
      const bad = styleDiffs.filter((d) => !allowHit(route, be, d.prop, d.from, d.to));
      st.styleDiff += styleDiffs.length;
      summary.styleDiff += styleDiffs.length;
      if (bad.length) {
        summary.unallowed += bad.length;
        for (const d of bad) unallowedLines.push(`STYLE ${route}  ${be.path}  .${be.cls}  ${d.prop}: ${d.from} → ${d.to}`);
      }
    }
    const badRect = rectDiffs.filter((d) => !d.allowed);
    if (rectDiffs.length) {
      st.rectDiff += rectDiffs.length;
      summary.rectDiff += rectDiffs.length;
      if (badRect.length) {
        summary.unallowed += badRect.length;
        for (const d of badRect) unallowedLines.push(`RECT  ${route}  ${be.path}  .${be.cls}  ${d.prop}: ${d.from} → ${d.to}`);
      }
    }
    if (!ONLY_CHANGED) {
      console.log(`[${route}] ${be.path}  .${be.cls}${clsChanged ? '  (cls→ .' + ae.cls + ')' : ''}`);
      for (const d of styleDiffs) console.log(`    style ${d.prop}: ${d.from} → ${d.to}`);
      for (const d of rectDiffs) console.log(`    rect  ${d.prop}: ${d.from} → ${d.to}${d.allowed ? '  [allow]' : '  ✗'}`);
    }
  }
  for (const [k, ae] of aMap) {
    if (bMap.has(k)) continue;
    st.added++; summary.added++;
    unallowedLines.push(`ADDED ${route}  ${ae.path}  .${ae.cls}`);
  }
  perRoute.push(st);
}

console.log('');
console.log('| 路由 | 元素 | CLASS-ONLY（改名） | STYLE-DIFF | RECT-DIFF | 白名单命中 | 缺失 | 新增 |');
console.log('|---|---|---|---|---|---|---|---|');
for (const r of perRoute) {
  console.log(`| ${r.route} | ${r.elements} | ${r.classOnly} | ${r.styleDiff} | ${r.rectDiff} | ${r.allowed} | ${r.missing} | ${r.added} |`);
}
console.log('');
console.log(`路由 ${summary.routes} / 对齐元素 ${totalEls}；CLASS-ONLY ${summary.classOnly}（改名零观感）、` +
  `STYLE-DIFF ${summary.styleDiff} / RECT-DIFF ${summary.rectDiff} 全部为有意变化（白名单 ${summary.allowed} 条）、` +
  `MISSING ${summary.missing} / ADDED ${summary.added}`);
if (Object.keys(allowedByRule).length) {
  console.log('白名单命中按理由：');
  for (const [why, n] of Object.entries(allowedByRule)) console.log(`  ${n} 条 — ${why}`);
}
if (unallowedLines.length) {
  console.error(`\n✗ 未归因差异 ${unallowedLines.length} 条：`);
  for (const l of unallowedLines.slice(0, 80)) console.error('  ' + l);
  process.exit(1);
}
console.log('\n✓ 无未归因差异');
