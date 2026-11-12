/**
 * 员工档案导入（卡 t_b0808afe）真机 UI 验收脚本 —— Playwright（隔离实例）。
 *
 * 前置：隔离后端（库 cp_hr_emp_import，:18089）+ vite dev（:5189，BACKEND_ORIGIN 指后端）。
 *
 * 跑法：
 *   cd frontend
 *   E2E_ADMIN_PASSWORD='...' EVIDENCE_DIR=.../docs/evidence/hr-emp-import \
 *     node ../../root/.../emp_import_ui.mjs        # 实际路径见调用方
 *
 * 覆盖（截图落 EVIDENCE_DIR）：
 *   01 工具栏两个按钮（下载导入模板 / 导入员工）
 *   02 点「下载导入模板」→ 真的下载到 xlsx
 *   03 导入对话框（选文件）
 *   04 预检报告（2 可导入 / 1 错误 + 错误明细表）
 *   05 确认导入后结果（成功 2 行 · 失败 1 行 + 「下载错误行」按钮）
 *   06 下载错误行 → rrorxlsx 落盘
 *   07 幂等：同一文件再传一次 → 预检 2 通过 / 确认后「已跳过 2 行（重复）」
 */
import { chromium, request } from '@playwright/test';
import { mkdirSync, readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

const BASE = process.env.E2E_BASE_URL ?? 'http://127.0.0.1:5189';
const PASSWORD = process.env.E2E_ADMIN_PASSWORD ?? 'Admin@123456';
const EVIDENCE = resolve(process.env.EVIDENCE_DIR ?? '/root/emp-import-e2e/ui');
const XLSX = process.env.E2E_XLSX ?? '/root/emp-import-e2e/员工花名册_3行.xlsx';

let pass = 0;
let fail = 0;
const checks = [];

function ok(name, detail = '') {
  pass += 1;
  checks.push(`[PASS] ${name}${detail ? `（${detail}）` : ''}`);
  console.log(checks.at(-1));
}

function bad(name, detail) {
  fail += 1;
  checks.push(`[FAIL] ${name}：${detail}`);
  console.log(checks.at(-1));
}

async function main() {
  mkdirSync(EVIDENCE, { recursive: true });
  console.log(`== 员工档案导入 UI 验收（${BASE} → ${EVIDENCE}）==`);

  // ── 登录（一次，拿 token 放进 storageState；后端登录有 10 次/60 秒限流） ──
  const api = await request.newContext({ baseURL: BASE });
  const res = await api.post('/api/system/auth/login', { data: { username: 'admin', password: PASSWORD } });
  const body = await res.json().catch(() => ({}));
  if (res.status() !== 200 || body?.code !== 0) {
    throw new Error(`登录失败：${res.status()} / ${JSON.stringify(body).slice(0, 200)}`);
  }
  if (body.data.must_change_password) {
    await api.put('/api/system/auth/password', {
      data: { old_password: PASSWORD, new_password: `${PASSWORD}-Ui9` },
      headers: { Authorization: `Bearer ${body.data.access_token}` },
    });
    const again = await api.post('/api/system/auth/login', {
      data: { username: 'admin', password: `${PASSWORD}-Ui9` },
    });
    Object.assign(body.data, (await again.json()).data);
  }
  const token = body.data.access_token;
  const refresh = body.data.refresh_token;
  await api.dispose();

  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({
    baseURL: BASE,
    viewport: { width: 1600, height: 1000 },
    acceptDownloads: true,
    storageState: {
      cookies: [],
      origins: [
        {
          origin: BASE,
          localStorage: [
            { name: 'x_access_token', value: token },
            { name: 'x_refresh_token', value: refresh },
            { name: 'cp-onboarding-done', value: '1' },
            { name: 'cloudpivot.setup-wizard.skipped:admin', value: '1' },
          ],
        },
      ],
    },
  });
  const page = await context.newPage();
  const errors = [];
  page.on('console', (msg) => {
    if (msg.type() === 'error') errors.push(msg.text());
  });

  await page.goto('/business/hr/employee', { waitUntil: 'domcontentloaded' });
  await page.waitForSelector('[data-testid="hr-emp-table"]', { timeout: 30_000 });

  // ── 01 工具栏 ──
  const templateBtn = page.locator('[data-testid="hr-emp-template"]');
  const importBtn = page.locator('[data-testid="hr-emp-import"]');
  await templateBtn.waitFor({ state: 'visible', timeout: 15_000 });
  await importBtn.waitFor({ state: 'visible', timeout: 15_000 });
  ok('01 工具栏出现「下载导入模板」与「导入员工」两个按钮');
  await page.screenshot({ path: join(EVIDENCE, '01-toolbar-import-buttons.png') });

  // ── 02 下载模板（真下载） ──
  const [download] = await Promise.all([page.waitForEvent('download', { timeout: 30_000 }), templateBtn.click()]);
  const templatePath = join(EVIDENCE, '员工档案导入模板.xlsx');
  await download.saveAs(templatePath);
  const suggested = download.suggestedFilename();
  const head = readFileSync(templatePath).subarray(0, 4).toString('hex');
  if (head === '504b0304') ok('02 模板下载成功（真 xlsx 容器）', `${suggested}，${head}`);
  else bad('02 模板下载', `文件头 ${head}（应为 504b0304）`);
  checks.push(`     建议文件名：${suggested}`);

  // ── 03 打开导入对话框并选文件 ──
  await importBtn.click();
  await page.waitForSelector('[data-testid="hr-emp-import-dialog"]', { timeout: 15_000 });
  await page.setInputFiles('[data-testid="hr-emp-import-file"]', XLSX);
  const fileNameText = (await page.locator('[data-testid="hr-emp-import-filename"]').innerText()).trim();
  if (fileNameText.includes('.xlsx')) ok('03 选中文件后显示文件名', fileNameText);
  else bad('03 文件名回显', fileNameText);
  await page.screenshot({ path: join(EVIDENCE, '02-import-dialog-pick-file.png') });

  // ── 04 上传预检 ──
  await page.click('[data-testid="hr-emp-import-upload"]');
  await page.waitForSelector('[data-testid="hr-emp-import-report"]', { timeout: 60_000 });
  const reportText = (await page.locator('[data-testid="hr-emp-import-report"]').innerText()).replace(/\s+/g, ' ');
  const errorRows = await page.locator('[data-testid="hr-emp-import-errors"] tbody tr').count();
  if (reportText.includes('可导入 2')) ok('04 预检报告显示「可导入 2」', reportText);
  else bad('04 预检报告', reportText);
  if (reportText.includes('错误 1')) ok('04 预检报告显示「错误 1」', reportText);
  else bad('04 预检错误数', reportText);
  if (errorRows >= 1) ok('04 错误明细表列出错误行', `${errorRows} 行`);
  else bad('04 错误明细表', '没有行');
  await page.screenshot({ path: join(EVIDENCE, '03-precheck-report.png'), fullPage: false });

  // ── 05 确认导入 ──
  await page.click('[data-testid="hr-emp-import-confirm"]');
  await page.waitForSelector('[data-testid="hr-emp-import-done"]', { timeout: 60_000 });
  const doneText = (await page.locator('[data-testid="hr-emp-import-done"]').innerText()).trim();
  if (doneText.includes('成功 2 行')) ok('05 结果：成功 2 行', doneText);
  else bad('05 结果文案', doneText);
  const downloadErrorsBtn = page.locator('[data-testid="hr-emp-import-download-errors"]');
  if (await downloadErrorsBtn.isVisible()) ok('05 「下载错误行」按钮出现（因为有 1 行错误）');
  else bad('05 「下载错误行」按钮', '不可见');
  await page.screenshot({ path: join(EVIDENCE, '04-import-done.png') });

  // ── 06 下载错误行（真下载） ──
  const [errDownload] = await Promise.all([page.waitForEvent('download', { timeout: 30_000 }), downloadErrorsBtn.click()]);
  const errorsPath = join(EVIDENCE, '员工档案导入错误行.xlsx');
  await errDownload.saveAs(errorsPath);
  const errHead = readFileSync(errorsPath).subarray(0, 4).toString('hex');
  if (errHead === '504b0304') ok('06 错误行 xlsx 下载成功', errDownload.suggestedFilename());
  else bad('06 错误行下载', errHead);

  // 关掉对话框，回列表确认新档案可见
  await page.keyboard.press('Escape').catch(() => {});
  await page.goto('/business/hr/employee', { waitUntil: 'domcontentloaded' });
  await page.waitForSelector('[data-testid="hr-emp-table"]', { timeout: 30_000 });
  await page.waitForTimeout(800);
  await page.screenshot({ path: join(EVIDENCE, '05-list-after-import.png') });

  // ── 07 幂等：同一文件再导一遍 ──
  await page.click('[data-testid="hr-emp-import"]');
  await page.waitForSelector('[data-testid="hr-emp-import-dialog"]', { timeout: 15_000 });
  await page.setInputFiles('[data-testid="hr-emp-import-file"]', XLSX);
  await page.click('[data-testid="hr-emp-import-upload"]');
  await page.waitForSelector('[data-testid="hr-emp-import-report"]', { timeout: 60_000 });
  await page.click('[data-testid="hr-emp-import-confirm"]');
  await page.waitForSelector('[data-testid="hr-emp-import-done"]', { timeout: 60_000 });
  const againText = (await page.locator('[data-testid="hr-emp-import-done"]').innerText()).trim();
  if (againText.includes('已跳过 2 行（重复）') && againText.includes('成功 0 行')) {
    ok('07 幂等：同一文件再导 → 成功 0 行 + 已跳过 2 行（重复）', againText);
  } else {
    bad('07 幂等提示', againText);
  }
  await page.screenshot({ path: join(EVIDENCE, '06-idempotent-skip.png') });

  if (errors.length > 0) {
    checks.push(`     浏览器控制台 error ${errors.length} 条（前 3 条）：${errors.slice(0, 3).join(' | ')}`);
  }

  await context.close();
  await browser.close();

  const fs = await import('node:fs');
  fs.writeFileSync(join(EVIDENCE, '00-ui-checks.txt'), checks.join('\n') + '\n', 'utf8');
  console.log(`\n== UI 结果：${pass} 通过 / ${fail} 失败 ==`);
  process.exit(fail === 0 ? 0 : 1);
}

main().catch((error) => {
  console.error('FATAL:', error);
  process.exit(2);
});
