// 静态等价断言（B3 版，基于 B2 的 equivalence-b2.mjs）
//
// 与 B2 的差异：
//   ① **支持 `NEW` 白名单**：B3 有两处 `NEW` 是「模板上新挂了变体属性」而非本地残留 ——
//      `hr/employee` 的 `panel-head__hint` 拿到 `margin-left:auto`（`--end`）与 `text-align:right`（`--right`），
//      `margin-left` 在 before 的生效值里根本不存在（本地从来没写过），所以只能以 NEW 表达；
//   ② 其余口径不变：before = 基线 ⊕ 页面本地；after = 基线 ⊕ 残留本地 ⊕ 模板上挂的变体。
//
// 用法：
//   node equivalence-b3.mjs <before.json> <after.json> <variants.json> [--allow allow.txt]
//     before.json / after.json 来自 `shell-variant-report.cjs --json`（before 在改造前的干净 worktree 上生成）
//     allow.txt 每行 `LOST|CHANGED|NEW  <cls>.<prop>`
import fs from 'node:fs';

const positional = process.argv.slice(2).filter((a) => a.endsWith('.json'));
const arg = (k, d = null) => { const i = process.argv.indexOf('--' + k); return i >= 0 ? process.argv[i + 1] : d; };
const [beforePath, afterPath, variantsPath] = positional;
const B = JSON.parse(fs.readFileSync(beforePath, 'utf8'));
const A = JSON.parse(fs.readFileSync(afterPath, 'utf8'));
const V = variantsPath ? JSON.parse(fs.readFileSync(variantsPath, 'utf8')) : {};

const allowFile = arg('allow');
const AL = { LOST: new Set(), CHANGED: new Set(), NEW: new Set() };
if (allowFile) {
  for (const raw of fs.readFileSync(allowFile, 'utf8').split('\n')) {
    const line = raw.replace(/#.*$/, '').trim();
    if (!line) continue;
    // `LOST|CHANGED|NEW  [file::]cls.prop` —— file 可选；带 file 时只放行那一页那一类
    const m = line.match(/^(LOST|CHANGED|NEW)\s+(?:([^:\s]+)::)?([a-z0-9_-]+)\.([a-z-]+)\s*$/);
    if (!m) continue;
    AL[m[1]].add((m[2] ? m[2] + '::' : '') + m[3] + '.' + m[4]);
  }
}
const allowHit = (set, file, cls, prop) => set.has(file + '::' + cls + '.' + prop) || set.has(cls + '.' + prop);

const basePropsBefore = (cls) => (B.baseline.classes[cls] ? B.baseline.classes[cls].props : null);
// **B3 与 B2 的关键差别**：本批会动基线本身（`__main{min-width:0}`、`title,meta{white-space:nowrap}`），
// 所以 after 的生效值必须取**改造后**的基线（A.baseline），否则仓库那 3 条会被误判成 LOST。
const basePropsAfter = (cls) => (A.baseline.classes[cls] ? A.baseline.classes[cls].props : (B.baseline.classes[cls] ? B.baseline.classes[cls].props : null));
const norm = (v) => String(v).toLowerCase().replace(/\s+/g, ' ').replace(/;$/, '').trim();
const normTok = (v) => norm(v).replace(/var\(\s*(--[a-z0-9-]+)\s*,[^)]*\)/g, 'var($1)');

function localMap(doc, file) {
  const out = new Map();
  for (const batch of Object.values(doc.batches)) {
    for (const b of batch.detail) {
      if (b.file !== file || !b.direct) continue;
      const cur = out.get(b.cls) || { props: {} };
      Object.assign(cur.props, b.props);
      out.set(b.cls, cur);
    }
  }
  return out;
}

const allFiles = new Set();
for (const batch of Object.values(B.batches)) for (const b of batch.detail) allFiles.add(b.file);

let lost = 0, changed = 0, added = 0, allowed = 0, classes = 0, touched = 0;
const rows = [];
const allowedRows = [];
for (const file of [...allFiles].sort()) {
  const lb = localMap(B, file);
  const la = localMap(A, file);
  const gone = [...lb.keys()].filter((c) => !la.has(c));
  if (!gone.length) continue;
  touched++;
  for (const [cls, bEntry] of lb) {
    classes++;
    const base = basePropsBefore(cls);
    if (!base) continue;
    const baseAfter = basePropsAfter(cls) || base;
    const effBefore = { ...base, ...bEntry.props };
    const varProps = V[file + '::' + cls] || {};
    const effAfter = { ...baseAfter, ...(la.get(cls)?.props || {}), ...varProps };
    for (const p of Object.keys(effBefore)) {
      if (!(p in effAfter)) {
        const key = cls + '.' + p;
        const row = { file, cls, kind: 'LOST', prop: p, before: effBefore[p] };
        if (allowHit(AL.LOST, file, cls, p)) { allowedRows.push(row); allowed++; } else { rows.push(row); lost++; }
        continue;
      }
      if (normTok(effBefore[p]) !== normTok(effAfter[p])) {
        const key = cls + '.' + p;
        const row = { file, cls, kind: 'CHANGED', prop: p, before: effBefore[p], after: effAfter[p] };
        if (allowHit(AL.CHANGED, file, cls, p)) { allowedRows.push(row); allowed++; } else { rows.push(row); changed++; }
      }
    }
    for (const p of Object.keys(effAfter)) {
      if (!(p in effBefore)) {
        const key = cls + '.' + p;
        const row = { file, cls, kind: 'NEW', prop: p, after: effAfter[p] };
        if (allowHit(AL.NEW, file, cls, p)) { allowedRows.push(row); allowed++; } else { rows.push(row); added++; }
      }
    }
  }
}

console.log(`比对 ${touched} 个「有壳类声明被删」的文件 / ${classes} 个类（变体表 ${Object.keys(V).length} 条）`);
console.log(`回归：LOST=${lost}  CHANGED=${changed}  NEW=${added}`);
console.log(`有意变化（白名单命中）：${allowed} 条`);
console.log('\n— 有意变化明细 —');
for (const r of allowedRows) console.log('  ' + JSON.stringify(r));
if (rows.length) {
  console.log('\n— 回归明细（必须为空）—');
  for (const r of rows) console.log('  ' + JSON.stringify(r));
}
process.exit(lost + changed + added === 0 ? 0 : 1);
