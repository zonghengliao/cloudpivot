#!/usr/bin/env node
/**
 * P1-6：另外 4 个主数据定义（计量单位 / 会计科目 / 仓库 / 库位）的真机导入验收。
 *
 * 与 `p16_import_page_check.mjs` 共用同一个隔离实例与同一套「模板 → 填行 → 上传 → 确认入库」链路，
 * 只是走 API（页面交互已在供应商那 6 项里验过，不必对每个定义重复点一遍）。
 *
 * 用法（从 frontend/ 目录跑）：
 *     cd frontend && node ../scripts/verify/p16_other_definitions_check.mjs
 */
import { execFileSync } from 'node:child_process';
import { readFileSync, writeFileSync } from 'node:fs';

const BASE = process.env.E2E_BASE_URL || 'http://127.0.0.1:5185';
const OUT = '/opt/cloudpivot/docs/evidence/20261104-p16-master-data-import';
const FILL = '/opt/cloudpivot/scripts/verify/p16_fill_template.py';
const PY = process.env.P16_PY || '/root/.hermes/profiles/backend-developer/cache/scratch/p16venv/bin/python';
const USER = process.env.P16_USER || 'admin';
const PASS_WORD = process.env.P16_PASSWORD || 'P16md@2026!';

let PASS = 0;
let FAIL = 0;
const ok = (m) => {
  PASS += 1;
  console.log(`[PASS] ${m}`);
};
const bad = (m) => {
  FAIL += 1;
  console.log(`[FAIL] ${m}`);
};
const expectEq = (name, want, got) => (String(want) === String(got) ? ok(`${name} = ${got}`) : bad(`${name}（期望 ${want}，实际 ${got}）`));

const login = await fetch(`${BASE}/api/system/auth/login`, {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({ username: USER, password: PASS_WORD }),
}).then((r) => r.json());
if (login.code !== 0) {
  console.error('登录失败：', JSON.stringify(login).slice(0, 200));
  process.exit(1);
}
const token = login.data.access_token;
const auth = { Authorization: `Bearer ${token}` };

async function template(bizType) {
  const file = `${OUT}/template-${bizType.replace('business.erp.', '')}.xlsx`;
  const res = await fetch(`${BASE}/api/business/impexp/import/template?biz_type=${encodeURIComponent(bizType)}`, { headers: auth });
  writeFileSync(file, Buffer.from(await res.arrayBuffer()));
  return file;
}

async function importFile(bizType, mode) {
  const tpl = await template(bizType);
  const file = `${OUT}/${bizType.replace('business.erp.', '')}-2rows.xlsx`;
  execFileSync(PY, [FILL, tpl, file, mode], { stdio: 'inherit' });
  const form = new FormData();
  form.append('file', new Blob([readFileSync(file)]), `${mode}.xlsx`);
  const start = Date.now();
  const up = await fetch(
    `${BASE}/api/business/impexp/import/upload?biz_type=${encodeURIComponent(bizType)}&mode=STRICT&duplicate=UPSERT`,
    { method: 'POST', body: form, headers: auth },
  ).then((r) => r.json());
  if (up.code !== 0) {
    bad(`${bizType} 上传失败：${JSON.stringify(up).slice(0, 200)}`);
    return 0;
  }
  expectEq(`${bizType} 预检失败行`, 0, up.data.report.error);
  const confirm = await fetch(`${BASE}/api/business/impexp/import/${up.data.task_id}/confirm`, { method: 'POST', headers: auth }).then((r) => r.json());
  if (confirm.code !== 0) {
    bad(`${bizType} 确认入库失败：${JSON.stringify(confirm).slice(0, 200)}`);
    return 0;
  }
  console.log(`  ${bizType}：预检 ${up.data.report.elapsed_ms} ms，全链路 ${Date.now() - start} ms`);
  return confirm.data.imported_rows;
}

expectEq('计量单位（辅助单位引用同文件的基准单位）入库行数', 2, await importFile('business.erp.unit', 'unit2'));
expectEq('会计科目（子级排在父级前面）入库行数', 2, await importFile('business.erp.account', 'account2'));
expectEq('仓库入库行数', 2, await importFile('business.erp.warehouse', 'warehouse2'));
expectEq('库位（挂在刚导入的仓库下）入库行数', 2, await importFile('business.erp.location', 'location2'));

// 幂等：同一批文件再导一次，行数不变（DB 口径由 psql 另测）
expectEq('计量单位重导行数（幂等）', 2, await importFile('business.erp.unit', 'unit2'));

console.log(`\n==== 其余 4 个定义：${PASS} passed / ${FAIL} failed ====`);
process.exit(FAIL === 0 ? 0 : 1);
