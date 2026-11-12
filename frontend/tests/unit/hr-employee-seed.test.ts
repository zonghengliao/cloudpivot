/**
 * 「从系统用户生成员工档案」（seed-from-users 前端化）单测 —— 跑在无 DOM 的 `tsx --test` 里。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * # 测什么（四层，越往下越容易出事）
 *
 * 1. **与后端逐条对拍**（读 `backend/src/business/hr/employee_seed.rs`）：路由、权限码、
 *    单次上限 `MAX_CANDIDATES`、6 个跳过原因码、`CREATE_FAILED`、`dry_run` 缺省 true。
 *    后端改一个字 / 一个数，这条先红 —— 而不是等 HR 点了「确认生成」才发现码值对不上。
 * 2. **汇总算术**：预览 / 已落库两种回执下的四桶数字、跳过分组的顺序与计数。
 *    面板上「将新建 N 人」是不是后端那个 N，全靠这一层。
 * 3. **两段式语义**：`canApplySeed` 只在「预览回执 + 有计划建档 + 没有请求在飞」时为真；
 *    执行回执（`dry_run=false`）**绝不能**再点一次「确认生成」。
 * 4. **接线**（逻辑对、模板写漏了也要红）：API 层传的 `dry_run` 是真是假、
 *    按钮是否挂 `canCreate`、`seedFromUsersApply` 是否**只有一个**调用点（在 `runSeedApply` 里）。
 *
 * 不渲染 `.vue`：本仓没有 happy-dom / jsdom（零新依赖），渲染层用源码断言兜住。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

import {
  SEED_ACTIONS,
  SEED_APPLIED_NOTICE,
  SEED_FAILED_REASON,
  SEED_MAX_CANDIDATES,
  SEED_PERM,
  SEED_PREVIEW_NOTICE,
  SEED_REASON_LABELS,
  SEED_ROUTE,
  SEED_SKIP_REASONS,
  applyButtonText,
  bucketLabel,
  canApplySeed,
  cellOrDash,
  emptyPlanHint,
  filterSeedRows,
  reasonLabel,
  rowActionLabel,
  rowActionTheme,
  seedHeadline,
  seedNoticeText,
  summarizeSeed,
  type SeedItem,
  type SeedResult,
} from '@/api/business/hr/employee-seed-kinds';

/** 后端实现（常量 / 原因码 / dry_run 缺省的唯一真源） */
const RUST_SEED = readFileSync(
  new URL('../../../backend/src/business/hr/employee_seed.rs', import.meta.url),
  'utf8',
);
/** 前端请求层（`dry_run` 真假与路径的接线） */
const API = readFileSync(new URL('../../src/api/business/hr/employee.ts', import.meta.url), 'utf8');
/** 页面源码（工具栏按钮 / 对话框 / 唯一调用点） */
const PAGE = readFileSync(new URL('../../src/views/business/hr/employee/index.vue', import.meta.url), 'utf8');

// ==================== 造数 ====================

/** 造一条预览行（默认「将建档」） */
function item(over: Partial<SeedItem> = {}): SeedItem {
  return {
    user_id: 'u-1',
    username: 'zhangsan',
    real_name: '张三',
    dept_id: 'd-1',
    dept_name: '研发部',
    action: 'CREATE',
    reason: null,
    message: '将建档：部门 研发部、入职日期 2026-01-15、状态 ACTIVE、工号落库时自动取号（预览不占号）',
    emp_no: null,
    employee_id: null,
    error_code: null,
    ...over,
  };
}

/** 造一份预览回执（dry-run：created 恒 0） */
function preview(over: Partial<SeedResult> = {}): SeedResult {
  const rows = over.preview ?? [
    item({ user_id: 'u-1', username: 'zhangsan', real_name: '张三' }),
    item({ user_id: 'u-2', username: 'lisi', real_name: '李四' }),
    item({
      user_id: 'u-3',
      username: 'wangwu',
      action: 'SKIP',
      reason: 'HAS_EMPLOYEE',
      message: '该账号已关联员工档 EMP-0003（王五）：无需再生成',
    }),
    item({
      user_id: 'u-4',
      username: 'robot',
      real_name: '',
      action: 'SKIP',
      reason: 'NOT_HUMAN',
      message: '账号 robot 的类型是「ROBOT」：员工档案只给人类账号',
    }),
    item({
      user_id: 'u-5',
      username: 'zhaoliu',
      real_name: '',
      dept_id: null,
      dept_name: '',
      action: 'SKIP',
      reason: 'NAME_MISSING',
      message: '账号 zhaoliu 没有真实姓名',
    }),
    item({
      user_id: 'u-6',
      username: 'sunqi',
      dept_id: null,
      dept_name: '',
      action: 'SKIP',
      reason: 'DEPT_MISSING',
      message: '账号 sunqi 没有部门，且本次未指定 dept_id',
    }),
  ];
  const counts = countActions(rows);
  return {
    dry_run: true,
    hire_date: '2026-01-15',
    dept_id: null,
    scanned: rows.length,
    planned_create: counts.CREATE,
    created: 0,
    skipped: counts.SKIP,
    failed: counts.FAILED,
    next_no: 'EMP-0007',
    preview: rows,
    ...over,
  };
}

/** 数一数回执里三种 action 各几条（测汇总时用来对拍） */
function countActions(rows: SeedItem[]): Record<string, number> {
  const out: Record<string, number> = { CREATE: 0, SKIP: 0, FAILED: 0 };
  for (const row of rows) {
    out[row.action] = (out[row.action] ?? 0) + 1;
  }
  return out;
}

/** 造一份「已落库」回执（create 行带回填的工号 + employee_id） */
function applied(over: Partial<SeedResult> = {}): SeedResult {
  const rows = [
    item({ user_id: 'u-1', username: 'zhangsan', emp_no: 'EMP-0007', employee_id: 'e-1', message: '已建档 EMP-0007' }),
    item({ user_id: 'u-2', username: 'lisi', emp_no: 'EMP-0008', employee_id: 'e-2', message: '已建档 EMP-0008' }),
    item({
      user_id: 'u-7',
      username: 'zhouba',
      action: 'FAILED',
      reason: 'CREATE_FAILED',
      message: '工号 EMP-0009 已被其他员工使用（本条已单独回滚、未占用工号）',
      error_code: 68001,
    }),
    item({ user_id: 'u-3', username: 'wangwu', action: 'SKIP', reason: 'HAS_EMPLOYEE', message: '已有档案' }),
  ];
  return {
    dry_run: false,
    hire_date: '2026-01-15',
    dept_id: null,
    scanned: rows.length,
    planned_create: 3,
    created: 2,
    skipped: 1,
    failed: 1,
    next_no: null,
    preview: rows,
    ...over,
  };
}

// ==================== 1. 与后端逐条对拍 ====================

test('契约常量与后端 employee_seed.rs 逐字一致', () => {
  assert.ok(
    RUST_SEED.includes(`pub const ROUTE_SEED: &str = "${SEED_ROUTE}";`),
    `后端应声明 ROUTE_SEED = ${SEED_ROUTE}`,
  );
  assert.ok(
    RUST_SEED.includes(`pub const PERM_SEED: &str = "${SEED_PERM}";`),
    `后端应声明 PERM_SEED = ${SEED_PERM}`,
  );
  assert.ok(
    RUST_SEED.includes(`pub const MAX_CANDIDATES: usize = ${SEED_MAX_CANDIDATES};`),
    `后端单次上限应为 ${SEED_MAX_CANDIDATES}`,
  );
});

test('6 个跳过原因码与后端 SeedSkip::code() 逐条一致（顺序即判定顺序）', () => {
  // 后端：`Self::HasEmployee => "HAS_EMPLOYEE",` 这种一行一个
  const pairs: Array<[string, string]> = [
    ['HasEmployee', 'HAS_EMPLOYEE'],
    ['NotHuman', 'NOT_HUMAN'],
    ['UserDisabled', 'USER_DISABLED'],
    ['NameMissing', 'NAME_MISSING'],
    ['NameTooLong', 'NAME_TOO_LONG'],
    ['DeptMissing', 'DEPT_MISSING'],
  ];
  assert.equal(SEED_SKIP_REASONS.length, pairs.length, '前端跳过原因码个数应与后端 6 个一致');
  SEED_SKIP_REASONS.forEach((code, index) => {
    const [variant, expected] = pairs[index];
    assert.equal(code, expected, `第 ${index + 1} 个原因码应是 ${expected}`);
    assert.ok(
      RUST_SEED.includes(`Self::${variant} => "${expected}",`),
      `后端 SeedSkip::${variant} 应映射到 "${expected}"`,
    );
  });
  // 每个码都要有中文短名（少一个面板上就会显示空标签）
  for (const code of SEED_SKIP_REASONS) {
    assert.ok(SEED_REASON_LABELS[code], `原因码 ${code} 缺中文短名`);
  }
});

test('失败原因码 / 三个动作码 / dry_run 缺省 true 与后端一致', () => {
  assert.ok(
    RUST_SEED.includes(`reason: Some("${SEED_FAILED_REASON}".to_string())`),
    '后端失败行的 reason 应为 CREATE_FAILED',
  );
  for (const action of SEED_ACTIONS) {
    // 后端用 `#[serde(rename_all = "SCREAMING_SNAKE_CASE")]`，枚举名是 Create/Skip/Failed
    assert.ok(
      RUST_SEED.includes('#[serde(rename_all = "SCREAMING_SNAKE_CASE")]'),
      '动作码应是 SCREAMING_SNAKE_CASE',
    );
  }
  assert.deepEqual([...SEED_ACTIONS], ['CREATE', 'SKIP', 'FAILED']);

  // 缺省 dry-run：后端 `unwrap_or(true)` —— 不传参数绝不许写库
  assert.ok(
    RUST_SEED.includes('self.dry_run.unwrap_or(true)'),
    '后端 is_dry_run 必须缺省 true',
  );
  assert.ok(SEED_PREVIEW_NOTICE.includes('不会写库'), '预览横幅必须明说「不会写库」');
});

test('入参 / 出参字段名与后端结构体逐条对得上（少一个就是运行时 undefined）', () => {
  // 出参
  for (const field of [
    'dry_run',
    'hire_date',
    'dept_id',
    'scanned',
    'planned_create',
    'created',
    'skipped',
    'failed',
    'next_no',
    'preview',
  ]) {
    assert.ok(
      new RegExp(`^\\s+pub ${field}:`, 'm').test(RUST_SEED),
      `后端 EmployeeSeedRes 应有字段 ${field}`,
    );
  }
  // 预览行
  for (const field of ['user_id', 'username', 'real_name', 'dept_id', 'dept_name', 'action', 'reason', 'message', 'emp_no', 'employee_id', 'error_code']) {
    assert.ok(
      new RegExp(`^\\s+pub ${field}:`, 'm').test(RUST_SEED),
      `后端 SeedItem 应有字段 ${field}`,
    );
  }
});

// ==================== 2. 汇总算术 ====================

test('预览回执：四桶数字取自后端，跳过按原因分组且顺序 = 后端判定顺序', () => {
  const res = preview();
  const summary = summarizeSeed(res);
  assert.equal(summary.scanned, 6);
  assert.equal(summary.plannedCreate, 2, '两条 CREATE 行');
  assert.equal(summary.created, 0, 'dry-run 下 created 恒 0');
  assert.equal(summary.skipped, 4);
  assert.equal(summary.failed, 0);
  assert.equal(summary.hasCreatable, true);
  // 分组顺序：HAS_EMPLOYEE → NOT_HUMAN → NAME_MISSING → DEPT_MISSING（后端判定顺序，不按条数排）
  assert.deepEqual(
    summary.skipBreakdown.map((row) => row.reason),
    ['HAS_EMPLOYEE', 'NOT_HUMAN', 'NAME_MISSING', 'DEPT_MISSING'],
  );
  assert.deepEqual(
    summary.skipBreakdown.map((row) => row.label),
    ['已有档案', '非人类账号', '缺真实姓名', '缺部门'],
  );
  assert.deepEqual(summary.failedBreakdown, [], '没有失败行时失败分组为空');
});

test('执行回执：成功 / 失败 / 跳过三个桶各自独立（不能只报「操作成功」）', () => {
  const res = applied();
  const summary = summarizeSeed(res);
  assert.equal(summary.created, 2);
  assert.equal(summary.failed, 1);
  assert.equal(summary.skipped, 1);
  assert.equal(summary.hasCreatable, true, '已建档行也算 CREATE 动作');
  assert.deepEqual(summary.failedBreakdown, [{ reason: 'CREATE_FAILED', label: '落库失败', count: 1 }]);
  assert.deepEqual(summary.skipBreakdown, [{ reason: 'HAS_EMPLOYEE', label: '已有档案', count: 1 }]);
});

test('后端将来新增前端还不认识的原因码：不丢计数、原样显示码值', () => {
  const res = preview({
    preview: [
      item({ user_id: 'u-9', action: 'SKIP', reason: 'BRAND_NEW_REASON', message: 'x' }),
      item({ user_id: 'u-10', action: 'SKIP', reason: 'HAS_EMPLOYEE', message: 'y' }),
    ],
    scanned: 2,
    planned_create: 0,
    skipped: 2,
  });
  const summary = summarizeSeed(res);
  const reasons = summary.skipBreakdown.map((row) => row.reason);
  assert.deepEqual(reasons, ['HAS_EMPLOYEE', 'BRAND_NEW_REASON'], '未知码追加在后，顺序稳定');
  assert.equal(reasonLabel('BRAND_NEW_REASON'), 'BRAND_NEW_REASON');
  assert.equal(reasonLabel(null), '');
});

test('分桶筛选与标签：预览叫「将新建」、落库后叫「已建档」', () => {
  const dry = preview();
  assert.equal(filterSeedRows(dry, 'create').length, 2);
  assert.equal(filterSeedRows(dry, 'skip').length, 4);
  assert.equal(filterSeedRows(dry, 'failed').length, 0);
  assert.equal(filterSeedRows(dry, 'all').length, dry.preview.length, 'all 原样返回，保持后端顺序');
  assert.equal(bucketLabel('create', true), '将新建');
  assert.equal(bucketLabel('create', false), '已建档');
  assert.equal(bucketLabel('all', true), '全部');

  const done = applied();
  assert.equal(filterSeedRows(done, 'create').length, 2);
  assert.equal(filterSeedRows(done, 'failed').length, 1);
});

// ==================== 3. 两段式语义 ====================

test('canApplySeed：只有「预览回执 + 有计划 + 不忙」才为真', () => {
  const dry = preview();
  assert.equal(canApplySeed(dry), true);
  assert.equal(canApplySeed(dry, true), false, '请求在飞时不许再点');
  assert.equal(canApplySeed(applied()), false, '执行回执不能再点确认（会二次写库）');
  assert.equal(canApplySeed(null), false, '没有回执（还没预览）不许点');
  // 计划为 0（全部已有档案）也不许点
  const nothing = preview({ planned_create: 0, preview: [], scanned: 3, skipped: 3 });
  assert.equal(canApplySeed(nothing), false);
});

test('按钮文案：N 与面板数字同源（0 人时不是「确认生成 0 人」）', () => {
  assert.equal(applyButtonText(preview()), '确认生成 2 人');
  assert.equal(applyButtonText(preview({ planned_create: 0, preview: [] })), '无可生成账号');
  assert.equal(applyButtonText(null), '无可生成账号');
});

test('结论句：预览 / 落库两种措辞', () => {
  const dry = preview();
  assert.equal(seedHeadline(dry), '扫描 6 人 · 将新建 2 人 · 跳过 4 人');
  const done = applied();
  assert.equal(seedHeadline(done), '已生成 2 人 · 跳过 1 人 · 失败 1 人');
  const failedPreview = preview({ failed: 1 });
  assert.equal(seedHeadline(failedPreview), '扫描 6 人 · 将新建 2 人 · 跳过 4 人 · 失败 1 人');
});

test('「将新建 0 人」的三种解释（卡面验收的第二遍预览）', () => {
  const allHasEmployee = preview({
    preview: [item({ user_id: 'u-3', action: 'SKIP', reason: 'HAS_EMPLOYEE', message: '已有档案 EMP-0003' })],
    scanned: 1,
    planned_create: 0,
    skipped: 1,
  });
  assert.match(emptyPlanHint(allHasEmployee), /全部已有员工档案（跳过 1 人）/);

  const emptyScope = preview({ preview: [], scanned: 0, planned_create: 0, skipped: 0 });
  assert.match(emptyPlanHint(emptyScope), /没有扫描到候选账号/);

  // 混合原因（真机第二遍预览就是这种：已建的变 HAS_EMPLOYEE，其余账号本身不满足条件）
  const mixed = preview({ planned_create: 0, skipped: 4 });
  assert.match(emptyPlanHint(mixed), /其中 1 人已有员工档案，其余 3 人见下表原因/);

  // 一个已有档案的都没有（全是账号本身不满足条件）
  const noEmployee = preview({
    preview: [item({ user_id: 'u-3', action: 'SKIP', reason: 'NOT_HUMAN', message: 'x' })],
    scanned: 1,
    planned_create: 0,
    skipped: 1,
  });
  assert.match(emptyPlanHint(noEmployee), /没有可新建的（跳过 1 人，原因见下表）/);
});

test('横幅文案：预览说「不会写库」、落库说「逐条事务」，两种口径不混用', () => {
  assert.equal(seedNoticeText(true), SEED_PREVIEW_NOTICE);
  assert.equal(seedNoticeText(false), SEED_APPLIED_NOTICE);
  assert.match(seedNoticeText(true), /不会写库/);
  assert.doesNotMatch(seedNoticeText(true), /已落库/, '预览绝不能出现「已落库」字样');
  assert.match(seedNoticeText(false), /已落库/);
});

test('行文案 / 配色 / 空值占位', () => {
  assert.equal(rowActionLabel(item(), true), '将建档');
  assert.equal(rowActionLabel(item({ action: 'SKIP', reason: 'HAS_EMPLOYEE' }), true), '跳过');
  assert.equal(rowActionLabel(item({ action: 'FAILED', reason: 'CREATE_FAILED' }), false), '失败');
  assert.equal(rowActionLabel(applied().preview[0], false), '已建档');
  assert.equal(rowActionTheme(item()), 'success');
  assert.equal(rowActionTheme(item({ action: 'SKIP' })), 'default', '跳过不是错误，别染红');
  assert.equal(rowActionTheme(item({ action: 'FAILED' })), 'warning');
  assert.equal(cellOrDash(''), '—');
  assert.equal(cellOrDash(null), '—');
  assert.equal(cellOrDash('  '), '—');
  assert.equal(cellOrDash('研发部'), '研发部');
});

// ==================== 4. 接线（源码断言） ====================

test('请求层：预览传 dry_run=true、执行传 false，路径写全 /api', () => {
  assert.ok(API.includes("const BASE = '/api/business/hr'"), 'BASE 必须带 /api 前缀（漏了会命中 SPA fallback 返回 HTML）');
  assert.match(
    API,
    /export function seedFromUsersPreview[\s\S]*?employee\/seed-from-users[\s\S]*?dry_run: true/,
    '预览函数必须显式传 dry_run: true',
  );
  assert.match(
    API,
    /export function seedFromUsersApply[\s\S]*?employee\/seed-from-users[\s\S]*?dry_run: false/,
    '执行函数必须传 dry_run: false',
  );
  assert.ok(API.includes(`export * from './employee-seed-kinds';`), '页面只认一个入口，kinds 要 re-export');
  // 路由由 `SEED_ROUTE` 拆出来：`${BASE}` = `/api/business/hr`，函数里补 `/employee/seed-from-users`
  assert.equal(SEED_ROUTE, '/business/hr/employee/seed-from-users');
  assert.ok(API.includes('`${BASE}/employee/seed-from-users`'), '两处请求都必须打这条路由（不能各写各的）');
  assert.equal(API.split('`${BASE}/employee/seed-from-users`').length - 1, 2, '预览 + 执行各一处');
});

test('页面：工具栏按钮挂权限码 + 三步对话框齐全 + 执行只有一个调用点', () => {
  assert.match(
    PAGE,
    /<t-button\s+v-if="canCreate"[\s\S]{0,200}?data-testid="hr-emp-seed"/,
    '「从系统用户生成」按钮必须挂在 canCreate 下（无权限不渲染）',
  );
  assert.ok(PAGE.includes('从系统用户生成</t-button>') || PAGE.includes('从系统用户生成'), '按钮文案');
  assert.match(PAGE, /<t-dialog[\s\S]{0,400}?data-testid="hr-emp-seed-dialog"/, '对话框要挂 testid');
  assert.ok(PAGE.includes('data-testid="hr-emp-seed-preview-btn"'), '参数步的「预览（不写库）」按钮');
  assert.ok(PAGE.includes('data-testid="hr-emp-seed-apply"'), '预览步的「确认生成」按钮');
  assert.ok(PAGE.includes(':disabled="!seedCanApply"'), '确认按钮必须由 canApplySeed 兜住');
  assert.ok(PAGE.includes('data-testid="hr-emp-seed-table"'), '明细表');
  assert.ok(PAGE.includes('seedStep === \'config\'') && PAGE.includes('seedStep === \'preview\''), '三步状态机');

  // 唯一调用点：apply 只在 runSeedApply 里出现（其余位置出现就是「一键写库」的风险）
  const applyCalls = PAGE.split('seedFromUsersApply(').length - 1;
  assert.equal(applyCalls, 1, `seedFromUsersApply 应只在 runSeedApply 里调用一次，实际 ${applyCalls} 处`);
  assert.match(
    PAGE,
    /async function runSeedApply[\s\S]*?seedFromUsersApply\(/,
    '那一处必须在 runSeedApply 里（用户点确认之后）',
  );
  // 预览函数可以在「参数步 + 重新预览」两处调用
  assert.ok(PAGE.split('seedFromUsersPreview(').length - 1 >= 1, '预览接口至少一处调用');
});
