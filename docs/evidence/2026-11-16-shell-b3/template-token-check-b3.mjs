// B3 模板自查：13 个改动 .vue 的 `<style>` 之前内容与基线提交逐字节比对。
//
// 允许的改动只有两种（都在「加类」这一个出口里，DOM 结构不变）：
//   ① 已有 `class="..."` 上**追加 token**（前缀不变，只加尾巴）；
//   ② **整行新增** `class="..."` 属性行 —— 本批给 `<ListPage>` 挂 `--dense` 就是这种：
//      那个组件标签原本没有 class 属性，只能新起一行（Vue 把 class 并到组件根节点 `.lp` 上）。
// 两者可以出现在同一个文件里（ledger 既有新增行、又有 `<div class="tool-row">` 追加 token）。
//
// 用法：node template-token-check-b3.mjs [--base <commit>]   （默认 facc0235 = B2 落地后的 HEAD）
import { execSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const argv = process.argv.slice(2);
const arg = (k, d = null) => { const i = argv.indexOf('--' + k); return i >= 0 ? argv[i + 1] : d; };
const BASE = arg('base', 'facc0235');
const root = '/opt/cloudpivot';

const files = [
  'erp/budget/mapping.vue',
  'erp/budget/report.vue',
  'erp/cost-alloc-rule/index.vue',
  'erp/cost-labor/index.vue',
  'erp/cost-overhead/index.vue',
  'erp/ledger/index.vue',
  'erp/receipt-refund/index.vue',
  'erp/report/payable-aging/index.vue',
  'erp/stock-out/index.vue',
  'erp/transfer/index.vue',
  'erp/warehouse/index.vue',
  'hr/employee/index.vue',
  'hr/salary/index.vue',
];

const CLASS_ONLY = /^\s*class="[^"]*"\s*$/;

/** 同一行、只有 class 取值变了、且新值是旧值的前缀扩展 → 返回追加的 token，否则 null */
function tokenAppend(oldLine, newLine) {
  const ma = oldLine.match(/class="([^"]*)"/);
  const mb = newLine.match(/class="([^"]*)"/);
  if (!ma || !mb) return null;
  if (oldLine.split(`class="${ma[1]}"`).join('\u0000') !== newLine.split(`class="${mb[1]}"`).join('\u0000')) return null;
  const ta = ma[1].split(/\s+/).filter(Boolean);
  const tb = mb[1].split(/\s+/).filter(Boolean);
  if (tb.length <= ta.length) return null;
  if (!ta.every((t, k) => tb[k] === t)) return null;
  return tb.slice(ta.length).join(' ');
}

let bad = 0;
let tokenLines = 0;
let attrLines = 0;
let unchanged = 0;
for (const f of files) {
  const rel = `frontend/src/views/business/${f}`;
  const head = execSync(`git -C ${root} show ${BASE}:${rel}`, { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
  const cur = fs.readFileSync(path.join(root, rel), 'utf8');
  const cut = (s) => (s.indexOf('<style') < 0 ? s : s.slice(0, s.indexOf('<style')));
  const a = cut(head).split('\n');
  const b = cut(cur).split('\n');
  if (a.join('\n') === b.join('\n')) { unchanged++; console.log(`OK    ${f}  模板+脚本逐字节未变`); continue; }

  const notes = [];
  let i = 0;
  let j = 0;
  let ok = true;
  let failMsg = '';
  while (i < a.length || j < b.length) {
    if (i < a.length && j < b.length && a[i] === b[j]) { i++; j++; continue; }
    // 先试「同行追加 token」（否则 `class="x"` → `class="x y"` 会被下面的「新增行」分支误吞）
    if (i < a.length && j < b.length) {
      const tok = tokenAppend(a[i], b[j]);
      if (tok) { notes.push(`第 ${j + 1} 行 class += "${tok}"`); tokenLines++; i++; j++; continue; }
    }
    if (j < b.length && CLASS_ONLY.test(b[j])) {
      notes.push(`新增 class 属性行 ${b[j].trim()}`);
      attrLines++; j++; continue;
    }
    ok = false;
    failMsg = `位置 a:${i + 1} b:${j + 1}\n  - ${JSON.stringify(a[i])}\n  + ${JSON.stringify(b[j])}`;
    break;
  }
  if (ok && i === a.length && j === b.length) {
    console.log(`OK    ${f}  ${notes.join('；')}`);
  } else {
    bad++;
    console.log(`FAIL  ${f}  ${failMsg}`);
  }
}
console.log(`\n结论：class token 追加 ${tokenLines} 行 + 新增 class 属性行 ${attrLines} 行；未变文件 ${unchanged} 个；异常 ${bad} 处`);
process.exit(bad ? 1 : 0);
