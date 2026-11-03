#!/usr/bin/env node
/**
 * P1-6 主数据 Excel 导入 —— 真机（隔离实例）页面验收。
 *
 * 前置：起隔离实例（空库 + 迁移种子，后端 18095 / vite 5185）
 *
 *     E2E_ISO_SRC_MODE=none E2E_ISO_FRESH=1 E2E_ISO_DB=cloudpivot_p16md_e2e \
 *     E2E_ISO_BACKEND_PORT=18095 E2E_ISO_FRONTEND_PORT=5185 \
 *     E2E_ISO_BACKEND_BIN=/opt/cloudpivot/backend/target/debug/cloudpivot \
 *     NIK_ADMIN_PASSWORD='Admin@123456' \
 *       bash scripts/verify/e2e_isolated_instance.sh up
 *
 * 用法（从 frontend/ 目录跑，方便解析 node_modules）：
 *
 *     cd frontend && node ../scripts/verify/p16_import_page_check.mjs
 *
 * 覆盖的验收项：
 *   1. 导入页下拉能看到 6 个主数据定义（供应商/物料/计量单位/会计科目/仓库/库位）
 *   2. 6 个定义都能下载模板（xlsx，三要素 sheet）
 *   3. 下载的模板填 3 行供应商 → 上传 → 校验通过 3 → 确认入库 → 供应商列表能看到
 *   4. 故意缺一行编码 → 报错带**行号 + 列名 + 原因**
 *   5. 重复导入同一份文件 → 幂等（供应商条数不变）
 *   6. 100 行物料走 API 上传 + 确认入库，实测耗时 < 5 秒（打印毫秒数）
 *
 * 断言只看 DOM / 接口返回；截图落 docs/evidence/20261104-p16-master-data-import/。
 */
import { execFileSync } from 'node:child_process';
import { mkdirSync, readFileSync, writeFileSync, statSync } from 'node:fs';

import { chromium } from '/opt/cloudpivot/frontend/node_modules/@playwright/test/index.mjs';

const BASE = process.env.E2E_BASE_URL || 'http://127.0.0.1:5185';
const OUT = '/opt/cloudpivot/docs/evidence/20261104-p16-master-data-import';
const FILL = '/opt/cloudpivot/scripts/verify/p16_fill_template.py';
const PY = process.env.P16_PY || '/root/.hermes/profiles/backend-developer/cache/scratch/p16venv/bin/python';
const USER = process.env.P16_USER || 'admin';
const PASS_WORD = process.env.P16_PASSWORD || 'Admin@123456';
// 首登强制改密（`must_change_password`）：这里改成脚本自己用的一次性口令。
// 换过之后再用 P16_PASSWORD 登录会失败，所以下面先试新口令、再试初始口令。
const NEW_PASS_WORD = process.env.P16_NEW_PASSWORD || 'P16md@2026!';

const OPTION_SELECTOR = '.t-select__list .t-select-option, .t-select-option, .t-select__dropdown li';

const BIZ = [
  ['business.erp.supplier', '供应商'],
  ['business.erp.material', '物料'],
  ['business.erp.unit', '计量单位'],
  ['business.erp.account', '会计科目'],
  ['business.erp.warehouse', '仓库'],
  ['business.erp.location', '库位'],
];

let PASS = 0;
let FAIL = 0;
const ok = (msg) => {
  PASS += 1;
  console.log(`[PASS] ${msg}`);
};
const bad = (msg) => {
  FAIL += 1;
  console.log(`[FAIL] ${msg}`);
};
const expectEq = (name, want, got) => {
  if (String(want) === String(got)) ok(`${name} = ${got}`);
  else bad(`${name}（期望 ${want}，实际 ${got}）`);
};
const expectTrue = (name, value) => {
  if (value) ok(name);
  else bad(name);
};

mkdirSync(OUT, { recursive: true });

const auth = (token) => ({ Authorization: `Bearer ${token}` });

async function downloadTemplate(token, bizType, file) {
  const res = await fetch(`${BASE}/api/business/impexp/import/template?biz_type=${encodeURIComponent(bizType)}`, {
    headers: auth(token),
  });
  expectEq(`${bizType} 模板 HTTP`, 200, res.status);
  const buf = Buffer.from(await res.arrayBuffer());
  writeFileSync(file, buf);
  const size = statSync(file).size;
  expectTrue(`${bizType} 模板是 xlsx 且非空（${size} 字节）`, size > 3000);
  return file;
}

function fillTemplate(template, out, mode) {
  execFileSync(PY, [FILL, template, out, mode], { stdio: 'inherit' });
  expectTrue(`生成 ${out}`, statSync(out).size > 3000);
  return out;
}

async function gotoImport(page) {
  await page.goto(`${BASE}/business/impexp/import`, { waitUntil: 'domcontentloaded' });
  await page.waitForSelector('.step-form__select', { timeout: 30_000 });
  // 兜底：任何引导气泡都不该挡住验收点击
  await page.addStyleTag({ content: '.onboarding{display:none!important}' });
  await page.waitForTimeout(400);
}

async function waitForText(page, text, timeoutMs = 20_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const body = await page.locator('body').innerText();
    if (body.includes(text)) return true;
    await page.waitForTimeout(400);
  }
  return false;
}

async function uploadAndConfirm(page, file, bizType, mode = 'STRICT') {
  // 上传：直接塞隐藏的 input[type=file]，再点「开始校验」
  await page.setInputFiles('input.file-input', file);
  const start = page.getByRole('button', { name: '开始校验' });
  await start.click();
  await page.waitForSelector('.summary', { timeout: 30_000 });
  const values = await page.locator('.summary__value').allInnerTexts();
  return {
    total: Number((values[0] || '0').replace(/,/g, '')),
    valid: Number((values[1] || '0').replace(/,/g, '')),
    error: Number((values[2] || '0').replace(/,/g, '')),
    elapsed: values[4] || '',
  };
}

(async () => {
  // ---------- 0. 登录（首登强制改密：先改密再用新口令登录） ----------
  const doLogin = async (password) => {
    const res = await fetch(`${BASE}/api/system/auth/login`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username: USER, password }),
    });
    return res.json();
  };
  let loginBody = await doLogin(NEW_PASS_WORD);
  if (loginBody.code !== 0) {
    loginBody = await doLogin(PASS_WORD);
    if (loginBody.code !== 0) {
      console.error('登录失败：', JSON.stringify(loginBody).slice(0, 300));
      process.exit(1);
    }
    const changeRes = await fetch(`${BASE}/api/system/auth/password`, {
      method: 'PUT',
      headers: {
        'Content-Type': 'application/json',
        Authorization: `Bearer ${loginBody.data.access_token}`,
      },
      body: JSON.stringify({ old_password: PASS_WORD, new_password: NEW_PASS_WORD }),
    });
    const changeBody = await changeRes.json();
    if (changeBody.code === 0) {
      ok('首登强制改密已完成（admin → 脚本专用口令）');
      loginBody = await doLogin(NEW_PASS_WORD);
      if (loginBody.code !== 0) {
        console.error('改密后重新登录失败：', JSON.stringify(loginBody).slice(0, 300));
        process.exit(1);
      }
    } else {
      console.error('改密失败：', JSON.stringify(changeBody).slice(0, 300));
      process.exit(1);
    }
  }
  const { access_token: accessToken, refresh_token: refreshToken } = loginBody.data;

  const browser = await chromium.launch();
  const context = await browser.newContext({ viewport: { width: 1500, height: 1100 } });
  await context.addInitScript(
    ([a, r]) => {
      localStorage.setItem('x_access_token', a);
      localStorage.setItem('x_refresh_token', r);
      localStorage.setItem('x_guide_done', '1');
      // 首屏引导气泡（`OnboardingHint.vue`）会挡住按钮点击：直接标成已看过
      localStorage.setItem('cp-onboarding-done', '1');
    },
    [accessToken, refreshToken],
  );
  const page = await context.newPage();
  const shot = async (name) => {
    await page.screenshot({ path: `${OUT}/${name}.png`, fullPage: true });
    console.log(`  shot → ${OUT}/${name}.png`);
  };

  // ---------- ① 6 个定义都在下拉里 + 6 个模板都能下 ----------
  await gotoImport(page);
  await page.locator('.step-form__select').click();
  await page.waitForTimeout(500);
  const options = await page.locator(OPTION_SELECTOR).allInnerTexts();
  const joined = options.join('|');
  for (const [, label] of BIZ) {
    expectTrue(`下拉里有「${label}」（实际列：${joined.slice(0, 80)}…）`.slice(0, 200), joined.includes(label));
  }
  await shot('01-definitions-dropdown');
  await page.keyboard.press('Escape');

  const templates = {};
  for (const [bizType] of BIZ) {
    templates[bizType] = await downloadTemplate(accessToken, bizType, `${OUT}/template-${bizType.replace('business.erp.', '')}.xlsx`);
  }

  // ---------- ② 3 行供应商：上传 → 校验 → 入库 → 列表可见 ----------
  await gotoImport(page);
  await page.locator('.step-form__select').click();
  await page.waitForTimeout(300);
  await page.locator(OPTION_SELECTOR).filter({ hasText: '供应商' }).first().click();
  await page.waitForTimeout(300);
  const goodFile = fillTemplate(templates['business.erp.supplier'], `${OUT}/supplier-3rows.xlsx`, 'supplier3');
  const report = await uploadAndConfirm(page, goodFile, 'business.erp.supplier');
  expectEq('供应商预检-总行数', 3, report.total);
  expectEq('供应商预检-通过', 3, report.valid);
  expectEq('供应商预检-失败', 0, report.error);
  console.log(`  预检耗时：${report.elapsed}`);
  await shot('02-supplier-precheck-pass');

  await page.getByRole('button', { name: '下一步：确认入库' }).click();
  await page.getByRole('button', { name: '确认入库' }).click();
  await page.waitForSelector('.t-alert--success, .t-alert--warning', { timeout: 30_000 });
  const confirmText = await page.locator('.step-body').innerText();
  expectTrue(`入库结果提示包含「入库 3 行」：${confirmText.replace(/\s+/g, ' ').slice(0, 160)}`, /入库\s*3\s*行/.test(confirmText));
  await shot('03-supplier-import-done');

  // 列表能看到（DOM 断言 + 接口条数）
  await page.goto(`${BASE}/business/erp/supplier`, { waitUntil: 'domcontentloaded' });
  await waitForText(page, 'P16MD-SUP1');
  const listText = await page.locator('body').innerText();
  for (const code of ['P16MD-SUP1', 'P16MD-SUP2', 'P16MD-SUP3']) {
    expectTrue(`供应商列表能看到 ${code}`, listText.includes(code));
  }
  await shot('04-supplier-list');

  const countSuppliers = async () => {
    const res = await fetch(`${BASE}/api/business/erp/supplier/page?page=1&page_size=100&keyword=P16MD`, {
      headers: auth(accessToken),
    });
    const body = await res.json();
    return (body.data.list || []).length;
  };
  expectEq('供应商接口条数', 3, await countSuppliers());

  // ---------- ③ 故意缺编码：报错带行号 + 列名 + 原因 ----------
  await gotoImport(page);
  await page.locator('.step-form__select').click();
  await page.waitForTimeout(300);
  await page.locator(OPTION_SELECTOR).filter({ hasText: '供应商' }).first().click();
  await page.waitForTimeout(300);
  const badFile = fillTemplate(templates['business.erp.supplier'], `${OUT}/supplier-bad-row.xlsx`, 'supplier_bad');
  const badReport = await uploadAndConfirm(page, badFile, 'business.erp.supplier');
  expectEq('坏文件-总行数', 3, badReport.total);
  expectEq('坏文件-失败行数', 2, badReport.error);
  // 等错误明细表格真正渲染出来（有失败行才会有这张表）
  await page.locator('.fail-table .t-table__body tr').first().waitFor({ timeout: 15_000 }).catch(() => {});
  await page.waitForTimeout(400);
  const failTable = await page.locator('.fail-table').innerText();
  console.log('  错误明细原文：', failTable.replace(/\s+/g, ' ').slice(0, 400));
  // 数据从第 3 行开始（模板第 2 行是示例行）：缺编码在第 4 行、负额度在第 5 行
  expectTrue('错误明细出现「第 4 行」（缺编码那一行）', failTable.includes('第 4 行'));
  expectTrue('错误明细点名列「供应商编码」', failTable.includes('供应商编码'));
  expectTrue('错误明细说明原因「必填项为空」', failTable.includes('必填项为空'));
  expectTrue('错误明细报出错码 REQUIRED', failTable.includes('REQUIRED'));
  expectTrue(
    '错误明细同时报出负数信用额度（第 5 行 / 超出范围）',
    failTable.includes('第 5 行') && failTable.includes('超出范围'),
  );
  await shot('05-supplier-error-rows');

  // ---------- ④ 重复导入同一份文件 → 幂等 ----------
  await gotoImport(page);
  await page.locator('.step-form__select').click();
  await page.waitForTimeout(300);
  await page.locator(OPTION_SELECTOR).filter({ hasText: '供应商' }).first().click();
  await page.waitForTimeout(300);
  await uploadAndConfirm(page, goodFile, 'business.erp.supplier');
  await page.getByRole('button', { name: '下一步：确认入库' }).click();
  await page.getByRole('button', { name: '确认入库' }).click();
  await page.waitForSelector('.t-alert--success, .t-alert--warning', { timeout: 30_000 });
  const again = await countSuppliers();
  expectEq('重复导入后供应商条数（幂等）', 3, again);
  await shot('06-supplier-reimport-idempotent');

  // ---------- ⑤ 100 行物料：API 走完整链路并计时 ----------
  const materialFile = fillTemplate(templates['business.erp.material'], `${OUT}/material-100rows.xlsx`, 'material100');
  const started = Date.now();
  const form = new FormData();
  form.append('file', new Blob([readFileSync(materialFile)]), 'material-100rows.xlsx');
  const uploadRes = await fetch(
    `${BASE}/api/business/impexp/import/upload?biz_type=business.erp.material&mode=STRICT&duplicate=UPSERT`,
    { method: 'POST', body: form, headers: auth(accessToken) },
  );
  const uploadBody = await uploadRes.json();
  if (uploadBody.code !== 0) {
    bad(`100 行物料上传失败：${JSON.stringify(uploadBody).slice(0, 200)}`);
  } else {
    expectEq('物料预检-总行数', 100, uploadBody.data.report.total);
    expectEq('物料预检-失败', 0, uploadBody.data.report.error);
    const confirmRes = await fetch(`${BASE}/api/business/impexp/import/${uploadBody.data.task_id}/confirm`, {
      method: 'POST',
      headers: auth(accessToken),
    });
    const confirmBody = await confirmRes.json();
    const elapsed = Date.now() - started;
    if (confirmBody.code === 0) {
      expectEq('物料入库行数', 100, confirmBody.data.imported_rows);
      const perCheck = uploadBody.data.report.elapsed_ms;
      console.log(`  100 行物料：预检 ${perCheck} ms，上传→预检→入库全链路 ${elapsed} ms`);
      expectTrue(`100 行物料全链路耗时 < 5000 ms（实测 ${elapsed} ms）`, elapsed < 5000);
      writeFileSync(
        `${OUT}/material-100rows-timing.txt`,
        `100 行物料导入计时（${new Date().toISOString()}）\n` +
          `预检 elapsed_ms = ${perCheck}\n` +
          `上传→预检→入库 confirm 全链路 = ${elapsed} ms\n` +
          `外键校验 SQL 条数 = ${uploadBody.data.report.fk_sql_statements}\n`,
      );
    } else {
      bad(`物料确认入库失败：${JSON.stringify(confirmBody).slice(0, 200)}`);
    }
  }

  // 物料列表能看到 100 行里的第一个
  await page.goto(`${BASE}/business/erp/material`, { waitUntil: 'domcontentloaded' });
  await waitForText(page, 'P16MD-MAT001');
  const materialList = await page.locator('body').innerText();
  expectTrue('物料列表能看到 P16MD-MAT001', materialList.includes('P16MD-MAT001'));
  await shot('07-material-list');

  await browser.close();
  console.log(`\n==== P1-6 验收：${PASS} passed / ${FAIL} failed ====`);
  process.exit(FAIL === 0 ? 0 : 1);
})().catch((error) => {
  console.error('验收脚本异常：', error);
  process.exit(1);
});
