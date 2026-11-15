// 由探针 JSON + 像素日志 + 换行点 JSON **机械生成**逐页核对表（B3）：不手抄数字。
//
// 用法：node gen-intentional-table.mjs <probe-diff.json> <pixels.log> <wrap-before.json> <wrap-after.json> > out.md
import fs from 'node:fs';

const [diffPath, pixelPath, wrapBeforePath, wrapAfterPath] = process.argv.slice(2);
const diffs = JSON.parse(fs.readFileSync(diffPath, 'utf8'));
const pixels = fs.readFileSync(pixelPath, 'utf8').split('\n');
const wrapB = JSON.parse(fs.readFileSync(wrapBeforePath, 'utf8')).routes;
const wrapA = JSON.parse(fs.readFileSync(wrapAfterPath, 'utf8')).routes;

const ORDER = [
  ['/business/erp/budget/mapping', 'budget/mapping', '`tool-row__type` 150px', '该页已挂 `--dense`；`__type` 吃基线 140'],
  ['/business/erp/budget/report', 'budget/report', '`tool-row__keyword` 200px', '吃基线 220'],
  ['/business/erp/cost-alloc-rule', 'cost-alloc-rule', '`tool-row__period` 140px', '+ `--dense`（140 不变，`gap` 不吃）'],
  ['/business/erp/cost-labor', 'cost-labor', '`tool-row__period` 132px', '+ `--dense` → 140'],
  ['/business/erp/cost-overhead', 'cost-overhead', '`tool-row__period` 132px', '+ `--dense` → 140'],
  ['/business/erp/ledger', 'ledger（Tab1 总账）', '`tool-row__period` 140px', '+ `--dense`（140 不变，`gap` 不吃）'],
  ['/business/erp/ledger?probe=trial', 'ledger（Tab2 科目余额表）', '同上（同一页）', '+ `--dense` → gap 12→8，period 140 不变'],
  ['/business/erp/receipt-refund', 'receipt-refund', '`tool-row__date` 140px', '+ `--dense`（140 不变）'],
  ['/business/erp/report/payable-aging', 'report/payable-aging', '`tool-row__period` 200 / `__date` 180', '吃基线 220 / 150'],
  ['/business/erp/stock-out', 'stock-out', '`tool-row__range` 240px', '吃基线 250'],
  ['/business/erp/transfer', 'transfer', '`tool-row__status` 140px', '吃基线 130'],
  ['/business/erp/warehouse', 'warehouse', '`panel-head__main/__title/__meta`', '三条提进基线（本地原样删）'],
  ['/business/hr/employee', 'hr/employee', '`panel-head__hint`（max-width/line-height/text-align）', '`--end`+`--right`；max-width/line-height 归一删'],
  ['/business/hr/salary', 'hr/salary', '`panel-head__meta` 13px', '吃基线 12px'],
  // 爆炸半径对照页（不该有几何变化）
  ['/business/erp/account', 'account（对照：基线爆炸半径）', '—（模板自带 panel-head）', '只吃基线 min-width/nowrap'],
  ['/business/erp/cost-period', 'cost-period（对照：UncollectedPanel）', '—', '只吃基线'],
  ['/business/erp/inventory', 'inventory（对照）', '—', '只吃基线'],
  ['/business/erp/price', 'price（对照）', '—（15px 标题归 B4）', '只吃基线'],
  ['/business/erp/report/report-item', 'report/report-item（对照）', '—（15px 标题归 B4）', '只吃基线'],
  ['/business/erp/sales-order', 'sales-order（对照：`hint--end`）', '—', '本批**没动** `--end`'],
  ['/business/erp/sales-return', 'sales-return（对照：`hint--end`）', '—', '本批**没动** `--end`'],
  ['/business/erp/shipment', 'shipment（对照：`hint--end`）', '—', '本批**没动** `--end`'],
];

const px = new Map();
for (const line of pixels) {
  const m = line.match(/^(business_\S+?\.png)\s+差异像素 (\d+)（mask 内）\s+有意义 (\d+)(.*)$/);
  if (!m) continue;
  px.set(m[1], { diff: Number(m[2]), meaningful: Number(m[3]), rest: m[4].trim() });
}
const slugOf = (route) => route.replace(/^\//, '').replace(/[/?=&]/g, '_') + '.png';

const geomByRoute = new Map();
const styleByRoute = new Map();
for (const d of diffs) {
  if (d.kind === 'GEOM' || d.kind === 'CLASS') {
    if (!geomByRoute.has(d.route)) geomByRoute.set(d.route, []);
    geomByRoute.get(d.route).push(d);
  } else if (d.kind === 'STYLE') {
    if (!styleByRoute.has(d.route)) styleByRoute.set(d.route, []);
    styleByRoute.get(d.route).push(d);
  }
}

const wrapChanged = (route) => {
  const b = wrapB[route] || [];
  const a = wrapA[route] || [];
  if (b.length !== a.length) return '工具行个数变了';
  for (let i = 0; i < b.length; i++) {
    if (b[i].lineCount !== a[i].lineCount) return `第${i + 1}行 ${b[i].lineCount}→${a[i].lineCount} 排`;
  }
  return null;
};

console.log('| # | 页面 | 删掉的本地声明 | 归到哪 | 探针几何差异（路由） | 像素（viewport / toolrow） | 换行点 |');
console.log('|--:|---|---|---|---|---|---|');
ORDER.forEach(([route, name, local, target], i) => {
  const g = (geomByRoute.get(route) || []).map((d) => `${(d.cls || '').split(/\s+/).filter((c) => /^(panel|tool|list)/.test(c)).join('.') || '?'}.${d.prop || d.kind} ${d.before ?? ''}→${d.after ?? ''}`);
  const s = px.get(slugOf(route));
  const st = px.get(slugOf(route).replace(/\.png$/, '.toolrow.png'));
  const pxTxt = s ? `${s.meaningful}${s.diff !== s.meaningful ? `（原 ${s.diff}）` : ''} / ${st ? st.meaningful : '—'}` : '—';
  const w = wrapChanged(route);
  console.log(`| ${i + 1} | ${name} | ${local} | ${target} | ${g.length ? g.join('；') : '无'} | ${pxTxt} | ${w ?? (wrapA[route]?.length ? '未变' : '无工具行')} |`);
});

console.log('\n> 像素列格式：`有意义差异像素数（含 mask 内全部）/ toolrow 裁切图同口径`；`0` = 零差异。');
