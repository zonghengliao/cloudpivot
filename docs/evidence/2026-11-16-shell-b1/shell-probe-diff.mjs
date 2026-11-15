// 前后快照 diff（B1 卡 t_79f92c08 验收第 4 条的判定器；B2–B6 复用）
//
// 用法：
//   node shell-probe-diff.mjs <before.json> <after.json> [--want "route:class,route:class"]
//
// 判定：
//   · 元素身份 = DOM 路径；一边有一边没有 → ELEMENT-MISSING / COUNT 差异（结构变了，必查）
//   · rect 的 x/y/w/h 任一变化 → GEOM（几何变了，本类收敛不允许）
//   · 40 个计算属性任一变化 → STYLE（值变化；需人工确认「是否伴随几何变化」）
//   · 对照组用法：同一份 dist 连跑两遍喂进来，结果必须全 0（否则探针本身有噪声，不算证据）
import fs from 'node:fs';

const positional = process.argv.slice(2).filter((a) => a.endsWith('.json'));
const before = JSON.parse(fs.readFileSync(positional[0], 'utf8'));
const after = JSON.parse(fs.readFileSync(positional[1], 'utf8'));

const diffs = [];
let compared = 0;
for (const route of Object.keys(before.routes)) {
  const b = before.routes[route].elements;
  const a = after.routes[route]?.elements ?? [];
  const byPath = new Map(a.map((e) => [e.path, e]));
  if (b.length !== a.length) diffs.push({ route, kind: 'COUNT', before: b.length, after: a.length });
  for (const eb of b) {
    const ea = byPath.get(eb.path);
    if (!ea) { diffs.push({ route, kind: 'ELEMENT-MISSING', path: eb.path, cls: eb.cls }); continue; }
    compared++;
    if (ea.cls !== eb.cls) diffs.push({ route, kind: 'CLASS', path: eb.path, before: eb.cls, after: ea.cls });
    for (const k of Object.keys(eb.rect)) {
      if (eb.rect[k] !== ea.rect[k]) diffs.push({ route, kind: 'GEOM', cls: eb.cls, prop: k, before: eb.rect[k], after: ea.rect[k], path: eb.path });
    }
    for (const p of Object.keys(eb.style)) {
      if (eb.style[p] !== ea.style[p]) diffs.push({ route, kind: 'STYLE', cls: eb.cls, prop: p, before: eb.style[p], after: ea.style[p], path: eb.path });
    }
  }
}

const geom = diffs.filter((d) => ['GEOM', 'COUNT', 'ELEMENT-MISSING', 'CLASS'].includes(d.kind));
const style = diffs.filter((d) => d.kind === 'STYLE');

console.log(`对比元素 ${compared} 个；几何/结构差异 ${geom.length} 条；样式值差异 ${style.length} 条`);

const group = (arr) => {
  const m = new Map();
  for (const d of arr) {
    const cls = d.cls ? d.cls.split(/\s+/).filter((c) => /^(panel|tool|list)/.test(c)).join('.') : '-';
    const k = `${cls} · ${d.prop || d.kind}`;
    m.set(k, (m.get(k) || 0) + 1);
  }
  return [...m.entries()].sort((x, y) => y[1] - x[1]);
};
console.log('\n几何/结构差异：');
if (!geom.length) console.log('  （无）');
for (const [k, n] of group(geom)) console.log(`  ${n}×  ${k}`);
console.log('\n样式值差异：');
if (!style.length) console.log('  （无）');
for (const [k, n] of group(style)) console.log(`  ${n}×  ${k}`);

fs.writeFileSync(positional[1].replace(/\.json$/, '.diff.json'), JSON.stringify(diffs, null, 1));
process.exit(geom.length ? 1 : 0);
