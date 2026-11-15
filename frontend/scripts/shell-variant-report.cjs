#!/usr/bin/env node
// =============================================================================
// 壳样式变体报告（shell variant report）
//
// 回答一个问题：**页面 `<style>` 里还剩哪些「壳类」（shell.less 定义的类 + panel-empty* 家族）
// 的本地定义，它们和全局基线差在哪、该走哪条收敛路径。**
//
// 背景：卡 t_3a87ffac 把与基线逐字相同的 248 块删进 `src/style/shell.less`。剩下的是**变体**，
// 合并任一条都会改观感，所以另立评审（卡 t_3d2fcc4d）。本脚本是该评审的度量工具：
// 不靠手写清单，直接从源码算出块数 / 变体 / 分批归属。
//
// 用法：
//   node scripts/shell-variant-report.cjs                  # 汇总表
//   node scripts/shell-variant-report.cjs --md  out.md     # 写 markdown 明细（逐批逐文件）
//   node scripts/shell-variant-report.cjs --json out.json  # 写完整结构化数据（含每条声明）
//   node scripts/shell-variant-report.cjs --batch "B2"     # 只看某一批
//   node scripts/shell-variant-report.cjs --strict         # 有非白名单变体时退出码 1
//   node scripts/shell-variant-report.cjs --baseline scripts/shell-variant-allowlist.json
//                                                          # 门禁档：新增页面壳类声明即退出码 1
//   node scripts/shell-variant-report.cjs --baseline scripts/shell-variant-allowlist.json --write
//                                                          # 重写快照（每落一批 / 削一条之后跑）
//
// 分批口径见 docs/architecture/shell-样式变体收敛-决策.md §5，门禁口径见 §2.4。
//
// `--baseline` 与 `--strict` 的区别：`--strict` 要求「页面零壳类声明」（B1–B6 全部落完之后
// 的终态），`--baseline` 是**存量快照档**——只拦「快照里没有的新声明」，存量按批次逐步削。
// 门禁先按 warn 档挂在 scripts/check.sh 的 L0（开关 SHELL_VARIANT_GATE）。
// =============================================================================

const fs = require('fs');
const path = require('path');
const crypto = require('crypto');

function findLess() {
  // 允许从仓库任意位置调用：脚本在 frontend/scripts/ 下，less 在 frontend/node_modules/
  const tries = [
    path.join(__dirname, '..', 'node_modules', 'less'),
    'less',
  ];
  for (const t of tries) {
    try { return require(t); } catch (e) { /* next */ }
  }
  throw new Error('找不到 less 模块：请在 frontend/ 下装好依赖（pnpm i）后重跑');
}
const less = findLess();

// ---------------- 参数 ----------------
const argv = process.argv.slice(2);
const opt = { src: path.join(__dirname, '..', 'src'), md: null, json: null, batch: null, strict: false, quiet: false, baseline: null, write: false };
for (let i = 0; i < argv.length; i++) {
  const a = argv[i];
  if (a === '--src') opt.src = path.resolve(argv[++i]);
  else if (a === '--md') opt.md = path.resolve(argv[++i]);
  else if (a === '--json') opt.json = path.resolve(argv[++i]);
  else if (a === '--batch') opt.batch = argv[++i];
  else if (a === '--baseline') opt.baseline = path.resolve(argv[++i]);
  else if (a === '--write') opt.write = true;
  else if (a === '--strict') opt.strict = true;
  else if (a === '--quiet') opt.quiet = true;
  else if (a === '--help' || a === '-h') {
    const lines = fs.readFileSync(__filename, 'utf8').split('\n');
    const out = [];
    for (let i = 1; i < lines.length; i++) {
      if (!lines[i].startsWith('//')) break; // 文件头注释结束（第一行非注释）
      out.push(lines[i].replace(/^\/\/ ?/, ''));
    }
    console.log(out.join('\n'));
    process.exit(0);
  } else { console.error('未知参数：' + a); process.exit(2); }
}
if (opt.write && !opt.baseline) { console.error('--write 必须与 --baseline <json> 一起用'); process.exit(2); }
const SRC = opt.src;

// ---------------- less 编译（剥离 @import，跨文件另行跟进） ----------------
function compile(css) {
  const imports = [];
  const stripped = css.replace(/@import\s+(?:\([^)]*\)\s*)?["']([^"']+)["']\s*;/g, (mm, p) => {
    if (!p.startsWith('~')) imports.push(p);
    return '';
  });
  let out = null, err = null;
  less.render(stripped, { silent: true, javascriptEnabled: false }, (e, r) => {
    if (e) err = e; else out = r.css;
  });
  if (out === null && err === null) err = new Error('less.render 未同步返回');
  return { css: out, error: err, imports };
}

// 扁平 CSS → [{selectors, decls, at}]
function parseFlatCss(css) {
  const clean = String(css).replace(/\/\*[\s\S]*?\*\//g, '');
  const rules = [];
  let token = '', cur = null;
  const atStack = [];
  for (let i = 0; i < clean.length; i++) {
    const c = clean[i];
    if (c === '{') {
      const prelude = token.trim(); token = '';
      if (prelude.startsWith('@') || prelude === '') { atStack.push(prelude); cur = null; }
      else {
        cur = { selectors: prelude.split(',').map((s) => s.trim().replace(/\s+/g, ' ')).filter(Boolean), decls: {}, at: atStack.filter(Boolean) };
        rules.push(cur);
      }
      continue;
    }
    if (c === '}') { token = ''; if (atStack.length) atStack.pop(); cur = null; continue; }
    if (c === ';' && cur) {
      const d = token.trim(); token = '';
      const idx = d.indexOf(':');
      if (idx > 0) cur.decls[d.slice(0, idx).trim()] = d.slice(idx + 1).trim().replace(/\s+/g, ' ');
      continue;
    }
    token += c;
  }
  return rules;
}

// ---------------- 1. 基线 ----------------
const shellLessPath = path.join(SRC, 'style', 'shell.less');
if (!fs.existsSync(shellLessPath)) { console.error('找不到基线：' + shellLessPath); process.exit(2); }
const shellCompiled = compile(fs.readFileSync(shellLessPath, 'utf8'));
if (shellCompiled.error) { console.error('shell.less 编译失败：' + shellCompiled.error.message); process.exit(2); }

const baseline = {};   // class -> { props, selector }
for (const r of parseFlatCss(shellCompiled.css)) {
  for (const sel of r.selectors) {
    const ms = sel.match(/\.([A-Za-z0-9_-]+)/g);
    if (!ms) continue;
    const cls = ms[ms.length - 1].slice(1);
    baseline[cls] = baseline[cls] || { props: {}, selector: sel };
    Object.assign(baseline[cls].props, r.decls);
  }
}
const FAMILY = /^(panel-head|panel-empty|tool-row|tool-card|list-card)(__|--|[.-]|$)/;
const isFamily = (c) => c in baseline || FAMILY.test(c);

// ---------------- 2. 扫描 ----------------
function walk(dir, acc = []) {
  if (!fs.existsSync(dir)) return acc;
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) walk(p, acc);
    else if (e.name.endsWith('.vue')) acc.push(p);
  }
  return acc;
}
const files = [...walk(path.join(SRC, 'views')), ...walk(path.join(SRC, 'components')), ...walk(path.join(SRC, 'layouts'))];
const appVue = path.join(SRC, 'App.vue');
if (fs.existsSync(appVue)) files.push(appVue);

function estimateLines(rawText, selText, declCount) {
  const esc = selText.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const m = new RegExp('[\\n;}]?[ \\t]*' + esc + '\\s*\\{').exec(rawText);
  if (!m) return declCount + 2;                   // 嵌套 &__x 之类：按「声明数 + 2 行」估
  const start = rawText.indexOf('{', m.index);
  let depth = 0;
  for (let j = start; j < rawText.length; j++) {
    if (rawText[j] === '{') depth++;
    else if (rawText[j] === '}') { depth--; if (depth === 0) return rawText.slice(m.index + 1, j + 1).split('\n').length; }
  }
  return declCount + 2;
}

const blocks = [];
const failures = [];
const imports = [];

function scan(css, rel, scoped, at) {
  const r = compile(css);
  if (r.error) { failures.push({ file: rel, error: String(r.error.message || r.error).slice(0, 200) }); return; }
  if (r.imports.length) imports.push({ from: rel, imports: r.imports });
  for (const rule of parseFlatCss(r.css)) {
    for (const sel of rule.selectors) {
      const found = (sel.match(/\.([A-Za-z0-9_-]+)/g) || []).map((s) => s.slice(1));
      const hits = found.filter(isFamily);
      if (!hits.length) continue;
      const lastSeg = sel.split(/\s+|>/).filter(Boolean).pop() || '';
      const lastClasses = (lastSeg.match(/\.([A-Za-z0-9_-]+)/g) || []).map((s) => s.slice(1));
      const direct = lastClasses.filter(isFamily).pop() || null;
      blocks.push({
        file: rel, scoped, at: rule.at.length ? rule.at.join(' ') : null, selector: sel,
        cls: direct || hits[hits.length - 1],
        direct: !!direct,
        props: Object.assign({}, rule.decls),
        lines: css ? estimateLines(css, sel, Object.keys(rule.decls).length) : Object.keys(rule.decls).length + 2,
      });
    }
  }
}

for (const f of files) {
  const text = fs.readFileSync(f, 'utf8');
  const rel = path.relative(SRC, f).split(path.sep).join('/');
  const re = /<style\b([^>]*)>([\s\S]*?)<\/style>/g;
  let m;
  while ((m = re.exec(text))) scan(m[2] || '', rel, /\bscoped\b/.test(m[1] || ''), (m[1] || '').trim());
}
const seen = new Set();
for (let k = 0; k < imports.length; k++) {
  for (const imp of imports[k].imports) {
    const base = path.dirname(path.join(SRC, imports[k].from));
    const p = path.resolve(base, imp);
    const hit = [p, p + '.less', p + '.css'].find((x) => fs.existsSync(x) && fs.statSync(x).isFile());
    if (!hit || seen.has(hit)) continue;
    seen.add(hit);
    scan(fs.readFileSync(hit, 'utf8'), path.relative(SRC, hit).split(path.sep).join('/') + ' (imported)', false, 'imported');
  }
}

// ---------------- 3. 与基线比对 ----------------
const norm = (v) => String(v).toLowerCase().replace(/\s+/g, ' ').replace(/;$/, '').trim();
// var(--x, #fallback) 与 var(--x) 视为同一 token（回退值不改变主题语义）
const normTok = (v) => norm(v).replace(/var\(\s*(--[a-z0-9-]+)\s*,[^)]*\)/g, 'var($1)');

for (const b of blocks) {
  const base = baseline[b.cls];
  if (!b.direct) { b.diff = { kind: 'context', missing: [], extra: [], changed: [] }; continue; }
  if (!base) {
    b.diff = { kind: 'not-in-baseline', missing: [], extra: Object.keys(b.props).map((p) => ({ prop: p, local: b.props[p] })), changed: [] };
    continue;
  }
  const missing = [], extra = [], changed = [];
  for (const p of Object.keys(base.props)) {
    if (!(p in b.props)) missing.push(p);
    else if (normTok(b.props[p]) !== normTok(base.props[p])) changed.push({ prop: p, local: b.props[p], base: base.props[p] });
  }
  for (const p of Object.keys(b.props)) if (!(p in base.props)) extra.push({ prop: p, local: b.props[p] });
  let kind;
  if (!missing.length && !extra.length && !changed.length) kind = 'same';
  else if (!missing.length && !changed.length) kind = 'superset';
  else if (changed.length && !missing.length) kind = 'conflict';
  else if (!changed.length && !extra.length) kind = 'subset';      // 只写了基线属性的一部分且值相同
  else kind = 'partial';                                          // 半个变体：缺属性 + 有冲突/额外
  b.diff = { kind, missing, extra, changed };
}

// ---------------- 4. 分批（收敛路径） ----------------
const SPACING = new Set(['gap', 'margin', 'margin-bottom', 'margin-top', 'margin-left', 'margin-right', 'column-gap', 'row-gap', 'padding']);
const SIZE = new Set(['width', 'min-width', 'max-width', 'font-size', 'line-height', 'letter-spacing', 'font-weight']);
const STRUCT = new Set(['display', 'flex-direction', 'align-items', 'justify-content', 'flex-wrap', 'flex', 'position']);
const TEXT = new Set(['text-align', 'white-space', 'text-overflow', 'vertical-align']);
const FOUR = new Set(['panel-empty', 'panel-empty__title', 'panel-empty__desc', 'tool-row__label']);
// ListPage 组件自持 scoped 样式：不并入全局（组件要自包含，见 shell.less 头部注释）
const SELF_HOSTED = ['components/ListPage/index.vue'];

const BATCHES = {
  B0: '组件自持（allowlist，不动）',
  B1: '纯冗余删除（零观感）',
  B2: '间距/颜色归一（2px 级，逐页截图）',
  B3: '尺寸/字号档位（width / font-size）',
  B4: '半变体补全（缺基线属性）',
  B5: '空态与标签变体族（panel-empty* + tool-row__label）',
  B6: '单页专有壳类（1-2 页）',
};

function batchOf(b) {
  const inBase = b.cls in baseline;
  if (SELF_HOSTED.includes(b.file)) return 'B0';
  if (/^panel-empty/.test(b.cls) || FOUR.has(b.cls)) {
    if (inBase && b.diff.kind === 'same') return 'B1';
    return 'B5';
  }
  if (!inBase) return 'B6';
  if (b.diff.kind === 'same') return 'B1';
  if (b.diff.kind === 'context') return 'B6';
  const ch = b.diff.changed.map((c) => c.prop), ex = b.diff.extra.map((c) => c.prop);
  // flex: 1 与 flex: 1 1 auto 对空 spacer 语义等价（基数为 0 时同解）
  if (ch.length === 1 && ch[0] === 'flex' && ex.length === 0) return 'B1';
  if (b.diff.missing.length) return 'B4';
  const all = [...ch, ...ex];
  if (all.some((p) => STRUCT.has(p))) return 'B4';
  if (all.every((p) => SIZE.has(p) || TEXT.has(p))) return 'B3';
  return 'B2';
}
for (const b of blocks) b.batch = batchOf(b);

// ---------------- 5. 汇总输出 ----------------
const groups = {};
for (const b of blocks) {
  const g = (groups[b.batch] = groups[b.batch] || { blocks: [], byFile: {}, estLines: 0 });
  g.blocks.push(b);
  (g.byFile[b.file] = g.byFile[b.file] || []).push(b.cls);
  g.estLines += b.lines;
}
const order = Object.keys(BATCHES);

if (!opt.quiet) {
  console.log(`壳样式变体报告 —— 基线 ${path.relative(process.cwd(), shellLessPath)}（${Object.keys(baseline).length} 类）`);
  console.log(`扫描 ${files.length} 个 .vue + ${seen.size} 个页面级 .less，命中 ${blocks.length} 块 / ${new Set(blocks.map((b) => b.file)).size} 文件 / 约 ${blocks.reduce((s, b) => s + b.lines, 0)} 行`);
  if (failures.length) console.log('编译失败 ' + failures.length + ' 处：' + JSON.stringify(failures));
  console.log('');
  console.log('| 批 | 块 | 文件 | 估算行 | 主要类 |');
  console.log('|---|---|---|---|---|');
  for (const k of order) {
    const g = groups[k];
    if (!g) continue;
    const cls = {};
    for (const b of g.blocks) cls[b.cls] = (cls[b.cls] || 0) + 1;
    const top = Object.entries(cls).sort((a, b) => b[1] - a[1]).slice(0, 5).map(([c, n]) => `${c}×${n}`).join('、');
    console.log(`| ${k} ${BATCHES[k]} | ${g.blocks.length} | ${Object.keys(g.byFile).length} | ${g.estLines} | ${top} |`);
  }
  if (opt.batch) {
    const k = opt.batch.toUpperCase();
    const g = groups[k];
    if (!g) { console.error('没有这一批：' + k); process.exit(2); }
    console.log('\n== ' + k + ' ' + BATCHES[k] + ' 明细 ==');
    for (const [f, cls] of Object.entries(g.byFile).sort()) console.log('  ' + f + '  — ' + cls.join('、'));
  }
}

if (opt.md) {
  const out = ['# 壳样式变体明细（`frontend/scripts/shell-variant-report.cjs --md` 产出，勿手改）', ''];
  for (const k of order) {
    const g = groups[k];
    if (!g) continue;
    out.push(`## ${k} ${BATCHES[k]} —— ${g.blocks.length} 块 / ${Object.keys(g.byFile).length} 文件 / 约 ${g.estLines} 行`, '');
    for (const [f, cls] of Object.entries(g.byFile).sort()) out.push(`- \`${f}\` — ${cls.join('、')}`);
    out.push('');
  }
  fs.writeFileSync(opt.md, out.join('\n'));
  if (!opt.quiet) console.log('\nmarkdown 明细 → ' + opt.md);
}

if (opt.json) {
  const data = {
    generatedAt: new Date().toISOString(),
    baseline: { file: path.relative(SRC, shellLessPath), classes: baseline },
    totals: { blocks: blocks.length, files: new Set(blocks.map((b) => b.file)).size, estLines: blocks.reduce((s, b) => s + b.lines, 0) },
    batches: Object.fromEntries(order.filter((k) => groups[k]).map((k) => [k, { label: BATCHES[k], blocks: groups[k].blocks.length, files: Object.keys(groups[k].byFile).sort(), estLines: groups[k].estLines, detail: groups[k].blocks }])),
    failures,
  };
  fs.writeFileSync(opt.json, JSON.stringify(data, null, 2));
  if (!opt.quiet) console.log('json → ' + opt.json);
}

// ---------------- 6. 门禁档（--baseline）：只拦「快照里没有的新声明」 ----------------
//
// 存量快照 = `scripts/shell-variant-allowlist.json`（本工具 --write 生成，勿手改）。
// 判定（每条 block 以 `file + selector` 为键）：
//   · 当前树有、快照没有        → `NEW <file> <selector>`，**退出码 1**（新长出来的壳类声明）
//   · 快照有、当前树已没有      → `FIXED <file> <selector>`，只提示（可以削 allowlist，不算失败）
//   · shell.less 的类清单哈希变了 → `BASELINE-DRIFT`，只提示（B2/B3 合法改基线，之后 --write 刷新）
if (opt.baseline) {
  const keyOf = (e) => e.file + '\u0000' + e.selector;
  const classHash = crypto
    .createHash('sha256')
    .update(Object.keys(baseline).sort().join('\n'))
    .digest('hex')
    .slice(0, 16);

  if (opt.write) {
    const entries = blocks
      .map((b) => ({ file: b.file, class: b.cls, selector: b.selector, batch: b.batch }))
      .sort((a, b) => (a.file + a.selector).localeCompare(b.file + b.selector));
    const doc = {
      _comment:
        '页面壳类声明存量快照（门禁 allowlist）。由 `node scripts/shell-variant-report.cjs --baseline scripts/shell-variant-allowlist.json --write` 生成，勿手改。' +
        '每落一批（B1…B6）后重新 --write，条目数只许降。终态只剩 B0 components/ListPage/index.vue。',
      shellLess: { file: path.relative(path.join(__dirname, '..'), shellLessPath).split(path.sep).join('/'), classCount: Object.keys(baseline).length, classesHash: classHash },
      totals: { entries: entries.length, byBatch: Object.fromEntries(order.filter((k) => groups[k]).map((k) => [k, groups[k].blocks.length])) },
      entries,
    };
    fs.writeFileSync(opt.baseline, JSON.stringify(doc, null, 2) + '\n');
    console.log(`allowlist 已重写 → ${path.relative(process.cwd(), opt.baseline)}（${entries.length} 条 / shell.less 类哈希 ${classHash}）`);
    process.exit(0);
  }

  if (!fs.existsSync(opt.baseline)) {
    console.error(`找不到存量快照：${opt.baseline}\n  先跑一次：node scripts/shell-variant-report.cjs --baseline ${path.relative(process.cwd(), opt.baseline)} --write`);
    process.exit(2);
  }
  const snap = JSON.parse(fs.readFileSync(opt.baseline, 'utf8'));
  const snapEntries = Array.isArray(snap.entries) ? snap.entries : [];
  const snapKeys = new Set(snapEntries.map(keyOf));
  const curKeys = new Set(blocks.map(keyOf));

  const added = blocks.filter((b) => !snapKeys.has(keyOf(b)));
  const fixed = snapEntries.filter((e) => !curKeys.has(keyOf(e)));
  const drift = snap.shellLess && snap.shellLess.classesHash !== classHash;

  if (!opt.quiet) {
    console.log(`门禁档：快照 ${snapEntries.length} 条 / 当前 ${blocks.length} 条（${path.relative(process.cwd(), opt.baseline)}）`);
  }
  if (drift) {
    console.log(
      `BASELINE-DRIFT  shell.less 类清单变了（快照 ${snap.shellLess.classCount} 类/${snap.shellLess.classesHash} → 当前 ${Object.keys(baseline).length} 类/${classHash}）` +
        '——改基线属 B2/B3 的合法动作，改完请重跑 --write 刷新快照',
    );
  }
  for (const b of added) console.log(`NEW   ${b.file}  ${b.selector}   [${b.batch}]`);
  for (const e of fixed) console.log(`FIXED ${e.file}  ${e.selector}   [${e.batch}]（可从 allowlist 削掉）`);

  if (added.length) {
    console.error(
      `\n✗ 新增 ${added.length} 处页面壳类声明（快照里没有）。页面必须零壳类声明：` +
        `\n  差异走命名变体（shell.less 里登记 + 属性写全），见 docs/architecture/shell-样式变体收敛-决策.md §2.1`,
    );
    process.exit(1);
  }
  console.log(`✓ 无新增页面壳类声明（存量 ${blocks.length} 条待按批削减，已删 ${fixed.length} 条可同步削 allowlist）`);
}

// --strict：未来做门禁用（B1–B6 落地后，除 B0 组件自持外任何页面壳类声明即失败）
if (opt.strict && !opt.baseline) {
  const debt = blocks.filter((b) => b.batch !== 'B0').length;
  if (debt) { console.error(`\n--strict：仍有 ${debt} 处非白名单变体（B0 组件自持 ${groups.B0 ? groups.B0.blocks.length : 0} 块不计）`); process.exit(1); }
}
