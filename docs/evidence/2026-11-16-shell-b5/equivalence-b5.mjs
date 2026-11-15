// 静态等价断言（B5 版，基于 B4 的 equivalence-b4.mjs）
//
// 与 B4 的差别（本批必然遇到）：
//   ① **before 的基线里没有这 4 个类**（`.panel-empty` / `__title` / `__desc` / `.tool-row__label`
//      在 B5 之前根本没进 shell.less，只有 `.panel-empty .panel-empty__icon` 一条）→
//      before 侧生效集只能取「本地声明 ⊕ 当时的基线」，基线缺失就是空集，不能像 B4 那样 `continue`；
//   ② **after 侧不能直接用 A.baseline**：报告工具把 shell.less 的所有选择器按「最后一个类」拍平，
//      于是 `panel-empty__icon` 会把 28（基线）/32（rich）/40（file,doc,lg）全并成一个终值。
//      本脚本改成**按页匹配选择器**：从编译后的 shell.less 里只取「该页元素真正命中的那些规则」，
//      拼出 `基线 ∪ 变体`；
//   ③ 变体归属（哪个页挂哪个 token）写在下面的 PAGE_TOKEN 表里，与 apply-b5.py 同一份口径。
//
// 用法：
//   node equivalence-b5.mjs report-before.json report-after.json [--allow allow.txt] [--json out.json]
import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';

const req = createRequire(path.join(process.cwd(), '__resolve__.js'));
const less = req(path.join(process.cwd(), 'node_modules', 'less'));
const argv = process.argv.slice(2);
const arg = (k, d = null) => { const i = argv.indexOf('--' + k); return i >= 0 ? argv[i + 1] : d; };
const positional = argv.filter((a, i) => a.endsWith('.json') && !['--json'].includes(argv[i - 1]));
const [beforePath, afterPath] = positional;

const B = JSON.parse(fs.readFileSync(beforePath, 'utf8'));
const A = JSON.parse(fs.readFileSync(afterPath, 'utf8'));
const SHELL = '/opt/cloudpivot/frontend/src/style/shell.less';

// ---------- 页面 → 模板上挂的 token ----------
const RICH = ['erp/customer', 'erp/material', 'erp/supplier', 'erp/unit', 'erp/warehouse', 'hr/employee'];
const FILE = ['impexp/export-task', 'impexp/import-task', 'storage/backend', 'storage/file'];
const DOC = ['erp/contract', 'erp/lot', 'erp/stock-out', 'erp/stock-take', 'hr/attendance', 'hr/leave'];
const LABEL_SM = ['erp/invoice', 'erp/receipt', 'erp/receivable', 'erp/receipt-refund', 'erp/contract', 'erp/cost-labor'];
const rel = (p) => 'views/business/' + p + '/index.vue';

const PAGE_TOKEN = {};   // file -> Set(class tokens on the page)
const add = (f, t) => { (PAGE_TOKEN[f] = PAGE_TOKEN[f] || new Set()).add(t); };
for (const p of RICH) add(rel(p), 'panel-empty--rich');
for (const p of FILE) add(rel(p), 'panel-empty--file');
for (const p of DOC) add(rel(p), 'panel-empty--doc');
for (const p of LABEL_SM) add(rel(p), 'tool-row__label--sm');
add('views/business/hr/salary/detail.vue', 'panel-empty--inline');
add('views/business/hr/salary/detail.vue', 'panel-empty--doc');
add('views/business/hr/employee/index.vue', 'panel-empty__icon--lg');
add('views/business/erp/expense/pay/index.vue', 'panel-empty__icon--success');

// ---------- shell.less → 规则（不拍平，保留选择器） ----------
let shellCss = '';
less.render(fs.readFileSync(SHELL, 'utf8'), { silent: true, javascriptEnabled: false }, (e, r) => {
  if (e) throw e;
  shellCss = r.css.replace(/\/\*[\s\S]*?\*\//g, '');
});
const RULES = [];
{
  let token = '', cur = null, at = [];
  for (const c of shellCss) {
    if (c === '{') {
      const pre = token.trim(); token = '';
      if (pre.startsWith('@')) { at.push(pre); cur = null; }
      else { cur = { sels: pre.split(',').map((s) => s.trim().replace(/\s+/g, ' ')).filter(Boolean), props: {}, at: [...at] }; RULES.push(cur); }
      continue;
    }
    if (c === '}') { token = ''; if (at.length) at.pop(); cur = null; continue; }
    if (c === ';' && cur) {
      const d = token.trim(); token = '';
      const i = d.indexOf(':');
      if (i > 0) cur.props[d.slice(0, i).trim()] = d.slice(i + 1).trim();
      continue;
    }
    token += c;
  }
}

// 该页某个元素命中的规则：选择器形如 `.a` / `.a .b`
// 关键：**元素自己的类**（own）与**祖先上下文**（anc）必须分开 ——
// `.panel-empty--rich .panel-empty__icon--lg` 里的 `--lg` 只属于图标元素，不能拿去匹配 __title。
function ownClasses(cls, file) {
  const own = new Set([cls]);
  for (const t of PAGE_TOKEN[file] || []) {
    // 修饰类归它的基类：`panel-empty--rich` 归 `panel-empty`；`panel-empty__icon--lg` 归 `panel-empty__icon`
    const base = t.replace(/--[a-z-]+$/, '');
    if (base === cls) own.add(t);
  }
  return own;
}
function ancClasses(cls, file) {
  const anc = new Set();
  if (/^panel-empty/.test(cls)) anc.add('panel-empty');
  for (const t of PAGE_TOKEN[file] || []) if (/^panel-empty--/.test(t)) anc.add(t);
  return anc;
}
function matchSel(sel, own, anc) {
  const parts = sel.split(' ');
  const classesOf = (p) => (p.match(/\.([A-Za-z0-9_-]+)/g) || []).map((s) => s.slice(1));
  if (parts.length === 1) {
    const c = classesOf(parts[0]);
    return c.length === 1 && own.has(c[0]);
  }
  if (parts.length === 2) {
    const a = classesOf(parts[0]);
    const b = classesOf(parts[1]);
    if (b.length !== 1 || !own.has(b[0])) return false;
    return a.every((c) => anc.has(c) || own.has(c));
  }
  return false;
}

// 页面上某个类「改造后」的生效声明集 = 命中该元素的规则按文件顺序叠加
function afterProps(file, cls) {
  const own = ownClasses(cls, file);
  const anc = ancClasses(cls, file);
  const out = {};
  for (const r of RULES) {
    if (r.at.length) continue;
    for (const sel of r.sels) {
      if (matchSel(sel, own, anc)) Object.assign(out, r.props);
    }
  }
  return out;
}

const norm = (v) => String(v).toLowerCase().replace(/\s+/g, ' ').replace(/;$/, '').trim();
const normTok = (v) => norm(v).replace(/var\(\s*(--[a-z0-9-]+)\s*,[^)]*\)/g, 'var($1)');

// 把 margin/padding 简写摊成四个长写，避免「页面写 margin-bottom:16px、变体写 margin:13px 0 16px」
// 被逐属性比对误判成 LOST + NEW。返回值是新对象（不改入参）。
function expandShorthand(props) {
  const out = { ...props };
  for (const key of ['margin', 'padding']) {
    const raw = out[key];
    if (raw == null) continue;
    delete out[key];
    const parts = String(raw).trim().split(/\s+/);
    const [t, r = t, b = t, l = r] = parts;
    out[key + '-top'] = t;
    out[key + '-right'] = r;
    out[key + '-bottom'] = b;
    out[key + '-left'] = l;
  }
  return out;
}

function localMap(doc, file) {
  const out = new Map();
  for (const batch of Object.values(doc.batches)) {
    for (const b of batch.detail) {
      if (b.file !== file || !b.direct) continue;
      const cur = out.get(b.cls) || { props: {}, selector: b.selector };
      Object.assign(cur.props, b.props);
      out.set(b.cls, cur);
    }
  }
  return out;
}

// ---------- 白名单 ----------
const AL = { LOST: new Set(), CHANGED: new Set(), NEW: new Set() };
const allowFile = arg('allow');
if (allowFile) {
  for (const raw of fs.readFileSync(allowFile, 'utf8').split('\n')) {
    const line = raw.replace(/#.*$/, '').trim();
    if (!line) continue;
    const m = line.match(/^(LOST|CHANGED|NEW)\s+(?:([^:\s]+)::)?([a-z0-9_-]+)\.([a-z-]+)\s*$/);
    if (!m) continue;
    AL[m[1]].add((m[2] ? m[2] + '::' : '') + m[3] + '.' + m[4]);
  }
}
const allowHit = (set, file, cls, prop) => set.has(file + '::' + cls + '.' + prop) || set.has(cls + '.' + prop);

// ---------- 主比对 ----------
const files = new Set();
for (const batch of Object.values(B.batches)) for (const b of batch.detail) files.add(b.file);

const rows = [], allowedRows = [];
let lost = 0, changed = 0, added = 0, allowed = 0, classes = 0, touched = 0;
for (const file of [...files].sort()) {
  const lb = localMap(B, file);
  const la = localMap(A, file);
  const gone = [...lb.keys()].filter((c) => !la.has(c));
  if (!gone.length) continue;
  touched++;
  for (const [cls, be] of lb) {
    classes++;
    const baseBefore = (B.baseline.classes[cls] || {}).props || {};
    const effBefore = expandShorthand({ ...baseBefore, ...be.props });
    const effAfter = expandShorthand({ ...afterProps(file, cls), ...(la.get(cls)?.props || {}) });
    const push = (kind, prop, bv, av) => {
      const row = { file, cls, kind, prop, before: bv, after: av };
      if (allowHit(AL[kind], file, cls, prop)) { allowedRows.push(row); allowed++; } else { rows.push(row); if (kind === 'LOST') lost++; else if (kind === 'CHANGED') changed++; else added++; }
    };
    for (const p of Object.keys(effBefore)) {
      if (!(p in effAfter)) push('LOST', p, effBefore[p], undefined);
      else if (normTok(effBefore[p]) !== normTok(effAfter[p])) push('CHANGED', p, effBefore[p], effAfter[p]);
    }
    for (const p of Object.keys(effAfter)) if (!(p in effBefore)) push('NEW', p, undefined, effAfter[p]);
  }
}

console.log(`比对 ${touched} 个「有壳类声明被删」的文件 / ${classes} 个类`);
console.log(`回归：LOST=${lost}  CHANGED=${changed}  NEW=${added}   有意变化（白名单命中）=${allowed}`);
const grp = (kind) => rows.filter((r) => r.kind === kind);
for (const kind of ['LOST', 'CHANGED', 'NEW']) {
  const g = grp(kind);
  console.log(`\n— ${kind}（${g.length}）—`);
  for (const r of g) console.log('  ' + r.file.replace('views/business/', '') + ' :: ' + r.cls + '.' + r.prop +
    (kind === 'CHANGED' ? `  ${r.before} → ${r.after}` : kind === 'LOST' ? `  (was ${r.before})` : `  = ${r.after}`));
}
const outJson = arg('json');
if (outJson) fs.writeFileSync(outJson, JSON.stringify({ totals: { lost, changed, added, allowed, files: touched, classes }, rows, allowedRows }, null, 1));
process.exit(lost + changed + added === 0 ? 0 : 1);
