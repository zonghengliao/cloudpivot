// 静态等价断言（B1 卡 t_79f92c08 验收第 3 条；照抄 shell-sink-evidence.md §3.3）
//
// 对每个改动文件、每个「改造前有本地声明」的壳类，算两边的**生效值**：
//   生效值 = 基线属性（shell.less）⊕ 本地属性（页面 scoped 覆盖，本地优先）
// 逐属性比对：
//   LOST    改造前生效、改造后没有            → 漏删/该保留的没保留
//   CHANGED 两边都有但值不同                  → 变更（本类收敛必须为 0，除非卡面明确列出）
//   NEW     改造后多出来的属性                → 级联补进来的（父卡踩过的坑，必须为 0）
//
// 用法：
//   node equivalence-static.mjs <report-before.json> <report-after.json> [--allow-changed flex] [--files a.vue,b.vue]
//
// 两个 json 来自 `shell-variant-report.cjs --json`：before 要在**改造前的树**上生成
//   （git worktree add --detach /tmp/wt <改造前 commit> && ln -s <本仓>/frontend/node_modules /tmp/wt/frontend/node_modules）
import fs from 'node:fs';

const positional = process.argv.slice(2).filter((a) => a.endsWith('.json'));
const arg = (k, d = null) => {
  const i = process.argv.indexOf('--' + k);
  return i >= 0 ? process.argv[i + 1] : d;
};
const allowProp = arg('allow-changed');
const beforePath = positional[0];
const afterPath = positional[1];

const B = JSON.parse(fs.readFileSync(beforePath, 'utf8'));
const A = JSON.parse(fs.readFileSync(afterPath, 'utf8'));

// 默认：改造前有、改造后没有本地声明的所有文件（= 本批真正动过的文件），无需手填清单
const allFiles = new Set();
for (const batch of Object.values(B.batches)) for (const b of batch.detail) allFiles.add(b.file);
const FILES = (arg('files') ? arg('files').split(',') : [...allFiles]).sort();

const baseProps = (cls) => (B.baseline.classes[cls] ? B.baseline.classes[cls].props : null);
const norm = (v) => String(v).toLowerCase().replace(/\s+/g, ' ').replace(/;$/, '').trim();
const normTok = (v) => norm(v).replace(/var\(\s*(--[a-z0-9-]+)\s*,[^)]*\)/g, 'var($1)');

function localMap(doc, file) {
  const out = new Map();
  for (const batch of Object.values(doc.batches)) {
    for (const b of batch.detail) {
      if (b.file !== file || !b.direct) continue; // 只看「最后一级简单选择器就是壳类」的直击声明
      const cur = out.get(b.cls) || { props: {}, selectors: [] };
      Object.assign(cur.props, b.props);
      cur.selectors.push(b.selector);
      out.set(b.cls, cur);
    }
  }
  return out;
}

let lost = 0, changed = 0, added = 0, allowed = 0, classes = 0, touched = 0;
const rows = [];
for (const file of FILES) {
  const lb = localMap(B, file);
  const la = localMap(A, file);
  const gone = [...lb.keys()].filter((c) => !la.has(c));
  if (!gone.length) continue; // 本批没动这个文件
  touched++;
  for (const [cls, bEntry] of lb) {
    classes++;
    const base = baseProps(cls);
    if (!base) continue; // 不在基线里的类不属批次收敛范围（B6 单页专有）
    const effBefore = { ...base, ...bEntry.props };
    const effAfter = { ...base, ...(la.get(cls)?.props || {}) };
    for (const p of Object.keys(effBefore)) {
      if (!(p in effAfter)) { rows.push({ file, cls, kind: 'LOST', prop: p, before: effBefore[p] }); lost++; continue; }
      if (normTok(effBefore[p]) !== normTok(effAfter[p])) {
        const ok = allowProp && p === allowProp;
        rows.push({ file, cls, kind: 'CHANGED', prop: p, before: effBefore[p], after: effAfter[p], allowed: !!ok });
        if (ok) allowed++; else changed++;
      }
    }
    for (const p of Object.keys(effAfter)) {
      if (!(p in effBefore)) { rows.push({ file, cls, kind: 'NEW', prop: p, after: effAfter[p] }); added++; }
    }
  }
}

console.log(`比对 ${touched} 个「有壳类声明被删」的文件 / ${classes} 个类`);
console.log(`LOST=${lost}  CHANGED=${changed}  NEW=${added}   （允许的 CHANGED（${allowProp || '无'}）=${allowed}）`);
for (const r of rows) console.log('  ' + JSON.stringify(r));
process.exit(lost + changed + added === 0 ? 0 : 1);
