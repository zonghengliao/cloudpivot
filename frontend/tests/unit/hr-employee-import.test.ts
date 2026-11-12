/**
 * 员工档案「Excel 导入」单测 —— 跑在无 DOM 的 `tsx --test` 里（卡 `t_b0808afe`）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * # 测什么（四层，越往下越容易出事）
 *
 * 1. **与后端逐条对拍**（读 `backend/src/business/impexp/builtin/hr/employee.rs`）：
 *    业务编码 `hr.employee`、行数上限 1000、列数 28、冲突策略是 `SKIP` 语义、
 *    「工号留空自动取号」。后端改一个字 / 一个数，这条先红。
 * 2. **计数算术**：`skippedDuplicateCount` / `importOutcomeText` —— 页面上
 *    「已跳过 N 行（重复）」是不是后端真正的跳过数，全靠这一层。
 * 3. **门禁**：`canConfirmImport`（有错行也允许确认，这是 PARTIAL 语义）、
 *    `canUseImport`（三个权限码缺一不可）、`validateImportFile`（扩展名 / 大小）。
 * 4. **接线**（逻辑对、模板写漏了也要红）：页面里两个按钮的 testid、
 *    API 层传的 `mode=PARTIAL` / `duplicate=SKIP`、模板下载走的是 HR 自己的路由、
 *    「下载错误行」按钮只在有错误行时出现。
 *
 * 不渲染 `.vue`：本仓没有 happy-dom / jsdom（零新依赖），渲染层用源码断言兜住。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

import {
  EMPLOYEE_IMPORT_BIZ_TYPE,
  EMPLOYEE_IMPORT_COLUMN_COUNT,
  EMPLOYEE_IMPORT_ERRORS_FILE,
  EMPLOYEE_IMPORT_MAX_ROWS,
  EMPLOYEE_IMPORT_PERMS,
  EMPLOYEE_IMPORT_TEMPLATE_FILE,
  EMPLOYEE_IMPORT_TEMPLATE_PATH,
  canConfirmImport,
  canDownloadErrors,
  canUseImport,
  errorColumnLabel,
  errorMessageText,
  importOutcomeText,
  skippedDuplicateCount,
  summarizeImport,
  validateImportFile,
} from '../../src/api/business/hr/employee-import-kinds';

/** 后端定义源码（逐条对拍用） */
const RUST = readFileSync(
  new URL('../../../backend/src/business/impexp/builtin/hr/employee.rs', import.meta.url),
  'utf8',
);
/** 后端 HR 模板路由源码 */
const RUST_HANDLER = readFileSync(
  new URL('../../../backend/src/business/hr/employee_import_handler.rs', import.meta.url),
  'utf8',
);
/** 前端 API 层 */
const API_SRC = readFileSync(
  new URL('../../src/api/business/hr/employee.ts', import.meta.url),
  'utf8',
);
/** 前端页面 */
const PAGE_SRC = readFileSync(
  new URL('../../src/views/business/hr/employee/index.vue', import.meta.url),
  'utf8',
);

// ==================== 1. 与后端对拍 ====================

test('业务编码 / 行数上限 / 列数与后端定义逐字一致', () => {
  assert.equal(EMPLOYEE_IMPORT_BIZ_TYPE, 'hr.employee');
  assert.match(RUST, /pub const EMPLOYEE_BIZ_TYPE: &str = "hr\.employee";/);
  assert.equal(EMPLOYEE_IMPORT_MAX_ROWS, 1000);
  assert.match(RUST, /pub const MAX_ROWS: usize = 1_000;/);
  // 列数 = 卡面 27 列 + 末尾追加的「工号」列
  assert.equal(EMPLOYEE_IMPORT_COLUMN_COUNT, 28);
  const columns = RUST.match(/static COLUMNS: &\[ImportColumn\] = &\[/);
  assert.ok(columns, '后端应有 COLUMNS 常量');
  const keys = RUST.match(/key: KEY_[A-Z_]+,/g) ?? [];
  assert.equal(keys.length, 28, `后端列定义应为 28 列，实际 ${keys.length}`);
  // 对拍：单测里也钉住「第 28 列是工号」
  assert.match(RUST, /header: "工号"/);
  assert.match(RUST, /remark: "可空；\*\*留空自动取号\*\*/);
});

test('模板与导入解析共用同一份列定义（不存在两份真相）', () => {
  // 后端 handler 必须从注册表拿定义、用 impexp 的 template 生成，不许手写表头
  assert.match(RUST_HANDLER, /global_registry\(\)/);
  assert.match(RUST_HANDLER, /template::build_template/);
  assert.match(RUST_HANDLER, /EMPLOYEE_BIZ_TYPE/);
  // 定义必须挂进 registry（漏挂 → 模板接口 60011）
  const builtin = readFileSync(
    new URL('../../../backend/src/business/impexp/builtin/mod.rs', import.meta.url),
    'utf8',
  );
  assert.match(builtin, /hr::employee::EmployeeImport/);
});

test('模板路由与权限码与卡面一致', () => {
  assert.equal(EMPLOYEE_IMPORT_TEMPLATE_PATH, '/api/business/hr/employee/import-template');
  assert.match(RUST_HANDLER, /"\/business\/hr\/employee\/import-template"/);
  assert.equal(EMPLOYEE_IMPORT_PERMS.template, 'business:hr:employee:import');
  assert.match(RUST_HANDLER, /permission = "business:hr:employee:import"/);
});

test('幂等：SKIP 策略 + 身份证唯一键（后端守卫都在）', () => {
  assert.match(RUST, /DuplicateStrategy::Skip => \{/);
  assert.match(RUST, /DO NOTHING/);
  assert.match(RUST, /ON CONFLICT \(org_id, id_card\) WHERE id_card IS NOT NULL AND deleted_at IS NULL/);
  // 已离职档案不许被一份旧花名册「复活」
  assert.match(RUST, /WHERE biz_employee\.status <> 'RESIGNED'/);
});

// ==================== 2. 计数算术 ====================

test('跳过数 = 预检通过数 − 实际入库数', () => {
  assert.equal(skippedDuplicateCount(2, 2), 0);
  assert.equal(skippedDuplicateCount(2, 0), 2, '同一份文件导第二遍：2 行全跳过');
  assert.equal(skippedDuplicateCount(3, 2), 1);
  // 后端多写了（不该发生）也不能算出负数
  assert.equal(skippedDuplicateCount(1, 2), 0);
});

test('结果文案：成功 / 跳过（重复）/ 失败 三种情况都写清楚', () => {
  const first = importOutcomeText({ task_id: 't', status: 'PARTIAL', imported_rows: 2, message: '共 1 批' }, 2, 1);
  assert.equal(first.imported, 2);
  assert.equal(first.skipped, 0);
  assert.equal(first.failed, 1);
  assert.equal(first.text, '成功 2 行 · 失败 1 行');

  const again = importOutcomeText({ task_id: 't', status: 'PARTIAL', imported_rows: 0, message: '共 1 批' }, 2, 0);
  assert.equal(again.skipped, 2);
  assert.match(again.text, /已跳过 2 行（重复）/, '卡面要求的原文');

  // 注意口径：`valid` **不含**校验失败的行（后端 report.total = valid + error），
  // 所以 failed 只用于文案，不参与跳过数的减法
  const mixed = importOutcomeText({ task_id: 't', status: 'PARTIAL', imported_rows: 1, message: '共 1 批' }, 2, 1);
  assert.equal(mixed.text, '成功 1 行 · 已跳过 1 行（重复） · 失败 1 行');
});

test('预检摘要覆盖示例行 / 空行', () => {
  const summary = summarizeImport({
    total: 3,
    valid: 2,
    error: 1,
    skipped_example: 1,
    skipped_blank: 2,
    warnings: [],
    errors: [],
    truncated: false,
    fk_sql_statements: 0,
    elapsed_ms: 12,
  });
  assert.equal(summary.valid, 2);
  assert.equal(summary.error, 1);
  assert.match(summary.text, /共 3 行/);
  assert.match(summary.text, /可导入 2 行/);
  assert.match(summary.text, /有错误 1 行/);
  assert.match(summary.text, /跳过示例行 1/);
  assert.match(summary.text, /跳过空行 2/);

  // 还没上传（report=null）时不炸
  const empty = summarizeImport(null);
  assert.equal(empty.total, 0);
  assert.equal(empty.text, '共 0 行 · 可导入 0 行');
});

test('错误行按 row_no + 序号取 key（同一行多条错误不撞 key）', () => {
  // 页面里的 importErrorRows 用 `${row_no}-${index}` 做 key；这里钉住口径
  assert.match(PAGE_SRC, /_key: `\$\{issue\.row_no\}-\$\{index\}`/);
  assert.match(PAGE_SRC, /row-key="_key"/);
});

// ==================== 3. 门禁与本地校验 ====================

test('canConfirmImport：有可导入行即可确认（有错行不拦，PARTIAL 语义）', () => {
  const withErrors = { total: 3, valid: 2, error: 1 } as never;
  assert.equal(canConfirmImport(withErrors), true, 'PARTIAL：错行不拦合法行');
  assert.equal(canConfirmImport({ total: 1, valid: 0, error: 1 } as never), false);
  assert.equal(canConfirmImport(null), false);
  assert.equal(canConfirmImport(withErrors, true), false, '请求在飞时不许重复点');
});

test('canUseImport：三个权限码缺一不可', () => {
  const all = new Set<string>([
    EMPLOYEE_IMPORT_PERMS.template,
    EMPLOYEE_IMPORT_PERMS.upload,
    EMPLOYEE_IMPORT_PERMS.confirm,
  ]);
  assert.equal(canUseImport((code) => all.has(code)), true);
  assert.equal(canUseImport(() => false), false);
  assert.equal(
    canUseImport((code) => code === EMPLOYEE_IMPORT_PERMS.template),
    false,
    '只有模板码时不能显示导入入口（上传会 403）',
  );
});

test('canDownloadErrors：有错误行才给按钮', () => {
  assert.equal(canDownloadErrors(0), false);
  assert.equal(canDownloadErrors(2), true);
});

test('validateImportFile：扩展名 / 5MB / 空文件', () => {
  const make = (name: string, size: number) => ({ name, size }) as File;
  assert.equal(validateImportFile(make('员工花名册.xlsx', 1024)), '');
  assert.equal(validateImportFile(make('花名册.XLS', 1024)), '');
  assert.match(validateImportFile(make('花名册.csv', 10)), /只支持 \.xlsx \/ \.xls/);
  assert.match(validateImportFile(make('花名册.xlsx', 6 * 1024 * 1024)), /超过 5MB/);
  assert.match(validateImportFile(make('花名册.xlsx', 0)), /空的/);
  assert.equal(validateImportFile(null), '');
});

test('错误明细列名 / 原因回退口径', () => {
  assert.equal(errorColumnLabel({ row_no: 5, column_header: '部门名称' }), '部门名称');
  assert.equal(errorColumnLabel({ row_no: 5, column_key: 'dept_name' }), 'dept_name');
  assert.equal(errorColumnLabel({ row_no: 5 }), '整行');
  assert.equal(errorMessageText({ row_no: 5, message: '部门不存在' }), '部门不存在');
  assert.equal(errorMessageText({ row_no: 5, error_message: '必填项为空' }), '必填项为空');
  assert.equal(errorMessageText({ row_no: 5 }), '-');
});

// ==================== 4. 接线（源码断言） ====================

test('API 层：显式 PARTIAL + SKIP，模板走 HR 路由，错误行走 errors/file', () => {
  assert.match(
    API_SRC,
    /uploadImportFile\(file, EMPLOYEE_IMPORT_BIZ_TYPE, 'PARTIAL', 'SKIP'\)/,
    '上传必须显式传 PARTIAL + SKIP（默认 STRICT + UPSERT 语义完全不同）',
  );
  assert.match(API_SRC, /service\.get<Blob>\(EMPLOYEE_IMPORT_TEMPLATE_PATH/);
  assert.match(API_SRC, /downloadImportErrors\(taskId\)/);
  // 模板文件名两份常量都在（页面用它们落盘）
  assert.equal(EMPLOYEE_IMPORT_TEMPLATE_FILE, '员工档案导入模板.xlsx');
  assert.equal(EMPLOYEE_IMPORT_ERRORS_FILE, '员工档案导入错误行.xlsx');
});

test('页面：两个入口按钮挂着卡面要求的 testid 与 canImport 门禁', () => {
  assert.match(PAGE_SRC, /data-testid="hr-emp-template"/);
  assert.match(PAGE_SRC, /data-testid="hr-emp-import"/);
  // 两个按钮都由 canImport 拦（无权限时按钮不渲染，而不是点了 403）
  const templateButton = PAGE_SRC.slice(PAGE_SRC.indexOf('data-testid="hr-emp-template"') - 200, PAGE_SRC.indexOf('data-testid="hr-emp-template"'));
  assert.match(templateButton, /v-if="canImport"/);
  const importButton = PAGE_SRC.slice(PAGE_SRC.indexOf('data-testid="hr-emp-import"') - 200, PAGE_SRC.indexOf('data-testid="hr-emp-import"'));
  assert.match(importButton, /v-if="canImport"/);
  // 下载模板按钮文案与动作
  assert.match(PAGE_SRC, /handleDownloadTemplate/);
  assert.match(PAGE_SRC, /downloadEmployeeImportTemplate\(\)/);
});

test('页面：导入对话框五步都在（选文件 / 预检 / 错误明细 / 确认 / 结果）', () => {
  for (const testid of [
    'hr-emp-import-dialog',
    'hr-emp-import-file',
    'hr-emp-import-upload',
    'hr-emp-import-report',
    'hr-emp-import-errors',
    'hr-emp-import-confirm',
    'hr-emp-import-done',
    'hr-emp-import-download-errors',
    'hr-emp-import-reset',
  ]) {
    assert.ok(PAGE_SRC.includes(`data-testid="${testid}"`), `缺 testid：${testid}`);
  }
  // 结果里必须有「已跳过 N 行（重复）」的来源（outcome.skipped 由 importOutcomeText 算）
  assert.match(PAGE_SRC, /importOutcomeText\(/);
  assert.match(PAGE_SRC, /importDone\.value = `导入完成：\$\{outcome\.value\.text\}/);
});

test('页面：确认入库成功后刷新列表（新档案要立刻可见）', () => {
  const confirmBlock = PAGE_SRC.slice(PAGE_SRC.indexOf('async function handleConfirmImport'));
  const body = confirmBlock.slice(0, confirmBlock.indexOf('async function handleDownloadErrors'));
  assert.match(body, /await loadRows\(\)/, '导入成功后必须刷新列表');
});
