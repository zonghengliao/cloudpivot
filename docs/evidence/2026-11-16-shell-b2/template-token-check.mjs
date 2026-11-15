// B2 模板自查：24 个 .vue 的 <style> 之前内容与 HEAD 逐字节比对，只允许 class 属性里追加 token
import { execSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';

const files = [
  'erp/asset-category/index.vue', 'erp/asset/index.vue', 'erp/budget/index.vue', 'erp/budget/mapping.vue',
  'erp/budget/report.vue', 'erp/cost-period/steps/UncollectedPanel.vue', 'erp/depreciation/index.vue',
  'erp/expense-type/index.vue', 'erp/expense/index.vue', 'erp/expense/pay/index.vue', 'erp/invoice/index.vue',
  'erp/ledger/index.vue', 'erp/price/index.vue', 'erp/receipt-refund/index.vue', 'erp/receipt/index.vue',
  'erp/receivable/index.vue', 'erp/report/balance-sheet/index.vue', 'erp/report/cash-flow/index.vue',
  'erp/report/income-statement/index.vue', 'erp/report/payable-aging/index.vue', 'erp/report/report-item/index.vue',
  'erp/sales-order/index.vue', 'erp/sales-return/index.vue', 'erp/shipment/index.vue',
];
const root = '/opt/cloudpivot';
let bad = 0;
let tokenLines = 0;
let unchanged = 0;
for (const f of files) {
  const rel = `frontend/src/views/business/${f}`;
  const head = execSync(`git -C ${root} show HEAD:${rel}`, { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
  const cur = fs.readFileSync(path.join(root, rel), 'utf8');
  const cut = (s) => (s.indexOf('<style') < 0 ? s : s.slice(0, s.indexOf('<style')));
  const a = cut(head).split('\n');
  const b = cut(cur).split('\n');
  if (a.join('\n') === b.join('\n')) { unchanged++; console.log(`OK  ${f}  模板+脚本逐字节未变`); continue; }
  if (a.length !== b.length) { console.log(`FAIL 行数不同 ${f}: ${a.length} vs ${b.length}`); bad++; continue; }
  for (let i = 0; i < a.length; i++) {
    if (a[i] === b[i]) continue;
    const ma = a[i].match(/class="([^"]*)"/);
    const mb = b[i].match(/class="([^"]*)"/);
    if (!ma || !mb) { console.log(`FAIL 非 class 行改动 ${f}:${i + 1}\n  - ${a[i]}\n  + ${b[i]}`); bad++; continue; }
    // 除 class 取值外，整行必须逐字节相同
    const restA = a[i].split(`class="${ma[1]}"`).join('\u0000');
    const restB = b[i].split(`class="${mb[1]}"`).join('\u0000');
    if (restA !== restB) { console.log(`FAIL class 之外还有改动 ${f}:${i + 1}\n  - ${a[i]}\n  + ${b[i]}`); bad++; continue; }
    const tokensA = ma[1].split(/\s+/).filter(Boolean);
    const tokensB = mb[1].split(/\s+/).filter(Boolean);
    const appended = tokensB.slice(tokensA.length);
    const samePrefix = tokensA.every((t, k) => tokensB[k] === t);
    if (!samePrefix || appended.length === 0) { console.log(`FAIL 不是纯追加 ${f}:${i + 1}  ${ma[1]} -> ${mb[1]}`); bad++; continue; }
    tokenLines++;
    console.log(`OK  ${f}:${i + 1}  class += "${appended.join(' ')}"`);
  }
}
console.log(`\n结论：模板改动 ${tokenLines} 行（全部为 class 追加）；未变文件 ${unchanged} 个；异常 ${bad} 处`);
process.exit(bad ? 1 : 0);
