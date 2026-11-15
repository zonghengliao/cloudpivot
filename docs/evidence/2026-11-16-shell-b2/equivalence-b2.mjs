// 静态等价断言（B2 版）—— 在 B1 那份（docs/evidence/2026-11-16-shell-b1/equivalence-static.mjs）基础上加两件事：
//   ① **变体可见**：本批把「页面本地声明」换成了「模板上的命名变体」，静态比对必须把变体的属性算进 after，
//      否则 `--end` 的 margin-left、`--dense` 的 gap、`--nowrap` 的 white-space 会全部误报 LOST；
//   ② **按「类.属性」白名单**：本批的「有意变化」是归一化的结果（gap→10 / margin-bottom→10 / color→placeholder /
//      tool-row 外边距消失），白名单里只放这 6 条，其余任何 LOST/CHANGED/NEW 都算回归。
//
// 生效值口径（与 B1 一致）：before = 基线 ⊕ 页面本地；after = 基线 ⊕ 残留本地 ⊕ 模板上挂的变体。
//
// 用法：
//   node equivalence-b2.mjs <before.json> <after.json> <variants.json> [--allow allow.txt]
//     before.json / after.json 来自 `shell-variant-report.cjs --json`（before 在改造前的树上生成）
//     variants.json 形如 { "views/.../x.vue::panel-head__tip": { "text-align": "right" }, ... }
//     allow.txt 每行 `LOST|CHANGED  <cls>.<prop>`；命中即计入「有意变化」，不计入回归
import fs from 'node:fs';

const positional = process.argv.slice(2).filter((a) => a.endsWith('.json'));
const arg = (k, d = null) => { const i = process.argv.indexOf('--' + k); return i >= 0 ? process.argv[i + 1] : d; };
const [beforePath, afterPath, variantsPath] = positional;
const B = JSON.parse(fs.readFileSync(beforePath, 'utf8'));
const A = JSON.parse(fs.readFileSync(afterPath, 'utf8'));
const V = variantsPath ? JSON.parse(fs.readFileSync(variantsPath, 'utf8')) : {};

const allowFile = arg('allow');
const AL_LOST = new Set();
const AL_CHANGED = new Set();
if (allowFile) {
  for (const line of fs.readFileSync(allowFile, 'utf8').split('\n')) {
    const m = line.match(/^(LOST|CHANGED)\s+([a-z0-9_-]+)\.([a-z-]+)\s*$/);
    if (!m) continue;
    (m[1] === 'LOST' ? AL_LOST : AL_CHANGED).add(m[2] + '.' + m[3]);
  }
}

const baseProps = (cls) => (B.baseline.classes[cls] ? B.baseline.classes[cls].props : null);
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
    const base = baseProps(cls);
    if (!base) continue;
    const effBefore = { ...base, ...bEntry.props };
    const varProps = V[file + '::' + cls] || {};
    const effAfter = { ...base, ...(la.get(cls)?.props || {}), ...varProps };
    for (const p of Object.keys(effBefore)) {
      if (!(p in effAfter)) {
        const key = cls + '.' + p;
        const row = { file, cls, kind: 'LOST', prop: p, before: effBefore[p] };
        if (AL_LOST.has(key)) { allowedRows.push(row); allowed++; } else { rows.push(row); lost++; }
        continue;
      }
      if (normTok(effBefore[p]) !== normTok(effAfter[p])) {
        const key = cls + '.' + p;
        const row = { file, cls, kind: 'CHANGED', prop: p, before: effBefore[p], after: effAfter[p] };
        if (AL_CHANGED.has(key)) { allowedRows.push(row); allowed++; } else { rows.push(row); changed++; }
      }
    }
    for (const p of Object.keys(effAfter)) {
      if (!(p in effBefore)) { rows.push({ file, cls, kind: 'NEW', prop: p, after: effAfter[p] }); added++; }
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
