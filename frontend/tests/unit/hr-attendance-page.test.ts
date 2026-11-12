/**
 * 考勤管理页（HR-29）单测：全部跑在无 DOM 的 `tsx --test` 里。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * # 测什么
 *
 * 1. **词表镜像**：`attendance-kinds.ts` 的两张「码值 → 中文」表（期间状态 3 / 来源 3）
 *    与后端 `attendance_period_dto.rs` / `attendance_record_dto.rs` 逐条对拍（直接读 Rust
 *    源码）。后端改取值而前端没跟 → 这里先红，而不是线上看到 `LOCKED` 这种码值。
 * 2. **状态机同域**：期间动作态必须与 `state_machine::ATTENDANCE_PERIOD_EDGES`
 *    的三条边一致（`DRAFT→SUBMITTED` / `SUBMITTED→LOCKED` / `LOCKED→DRAFT`），
 *    不能有「草稿直接锁定」这种后端会 400 的按钮。
 * 3. **上限对齐**：天数 31 / 小时 744 / 次数 62 / 跨度 31 天 / 解锁原因 5 字，
 *    逐条与后端 DTO 常量对拍（改一边漏另一边 = 用户在界面上被放过去、到后端才 400）。
 * 4. **口径**：`Decimal` 字符串去尾零（`22.00` → `22`）、`t-input-number` 回的是 number、
 *    单元格失焦只发**一个**字段、值没变不发请求；`status` 多选逗号串；
 *    `period_id` 缺失时不打注定 400 的请求。
 * 5. **接线**：`index.vue` 是否真的用了这些纯函数、真的下发 `period_id`、真的走
 *    `patchRecord` / `uploadAttendanceImport`；`router` 是否注册了 `/business/hr/attendance`。
 *
 * 不渲染 `.vue`：本仓没有 happy-dom/jsdom（零新依赖），渲染层用源码断言兜住。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

import {
  ATTENDANCE_BIZ_TYPE,
  ATTENDANCE_PERIOD_COLUMNS,
  ATTENDANCE_PERMS,
  ATTENDANCE_RECORD_COLUMNS,
  LEDGER_PERIOD_STATUSES,
  MAX_SPAN_DAYS,
  MIN_UNLOCK_REASON,
  NUMBER_FIELDS,
  PERIOD_CODE_LEN,
  PERIOD_MAX_REMARK,
  PERIOD_STATUS_LABELS,
  PERIOD_STATUS_OPTIONS,
  PERIOD_STATUS_VALUES,
  SOURCE_LABELS,
  SOURCE_VALUES,
  buildPeriodQuery,
  buildRecordQuery,
  cellText,
  datetimeText,
  decimalText,
  decimalValue,
  emptyPeriodFilter,
  emptyPeriodForm,
  emptyRecordFilter,
  emptyTotals,
  isDateText,
  isPeriodCode,
  isPeriodFiltered,
  isRecordFiltered,
  numberFieldMeta,
  periodActionsOf,
  periodAuditText,
  periodFormFromRow,
  periodOptionLabel,
  periodOptions,
  periodRangeText,
  periodStatusLabel,
  periodStatusTheme,
  recordPatchOf,
  sourceLabel,
  spanDaysOf,
  statusChangePayload,
  textOr,
  toPeriodCreatePayload,
  toPeriodUpdatePayload,
  totalsOfPeriod,
  totalsText,
  validatePeriodForm,
  validateUnlockReason,
  type AttendancePeriodRow,
  type AttendanceRecordRow,
} from '../../src/api/business/hr/attendance-kinds';

const RUST_PERIOD_DTO = readFileSync(
  new URL('../../../backend/src/business/hr/attendance_period_dto.rs', import.meta.url),
  'utf8',
);
const RUST_RECORD_DTO = readFileSync(
  new URL('../../../backend/src/business/hr/attendance_record_dto.rs', import.meta.url),
  'utf8',
);
const RUST_STATE_MACHINE = readFileSync(
  new URL('../../../backend/src/business/shared/state_machine.rs', import.meta.url),
  'utf8',
);
const SQL_HR_TABLES = readFileSync(
  new URL('../../../backend/migrations/business/V0383__hr_tables.up.sql', import.meta.url),
  'utf8',
);
const SQL_HR_MENU = readFileSync(
  new URL('../../../backend/migrations/business/V0387__hr_menu_perms.up.sql', import.meta.url),
  'utf8',
);
const PAGE = readFileSync(
  new URL('../../src/views/business/hr/attendance/index.vue', import.meta.url),
  'utf8',
);
const API = readFileSync(new URL('../../src/api/business/hr/attendance.ts', import.meta.url), 'utf8');
const ROUTER = readFileSync(new URL('../../src/router/index.ts', import.meta.url), 'utf8');

// ==================== ① 词表镜像 ====================

/** 期间状态：三个码值与中文名逐条对拍后端 `status_label` 的 match 分支 */
test('period status vocabulary mirrors attendance_period_dto.rs', () => {
  assert.deepEqual([...PERIOD_STATUS_VALUES], ['DRAFT', 'SUBMITTED', 'LOCKED']);
  assert.deepEqual(
    PERIOD_STATUS_OPTIONS.map((item) => item.value),
    [...PERIOD_STATUS_VALUES],
    '下拉顺序必须与后端常量同序',
  );
  for (const code of PERIOD_STATUS_VALUES) {
    // 后端：`STATUS_DRAFT => "草稿",` 这类 match 分支
    const branch = new RegExp(`STATUS_${code}\\s*=>\\s*"([^"]+)"`).exec(RUST_PERIOD_DTO);
    assert.ok(branch, `后端 status_label 缺少 ${code} 分支`);
    assert.equal(PERIOD_STATUS_LABELS[code], branch[1], `${code} 的中文名与后端不一致`);
  }
  // 状态域必须在库层 CHECK 里逐字出现（前端多一个值 = 后端 400）
  const check = /CONSTRAINT ck_biz_att_period_status CHECK \(status IN \(([^)]+)\)\)/.exec(SQL_HR_TABLES);
  assert.ok(check, 'V0383 的期间状态 CHECK 没找到');
  for (const code of PERIOD_STATUS_VALUES) {
    assert.ok(check[1].includes(`'${code}'`), `库层 CHECK 缺 ${code}`);
  }
  assert.equal((check[1].match(/'/g) ?? []).length / 2, PERIOD_STATUS_VALUES.length, '库层状态值多了');
});

/** 来源：三个码值与中文名逐条对拍后端 `source_label` */
test('record source vocabulary mirrors attendance_record_dto.rs', () => {
  assert.deepEqual([...SOURCE_VALUES], ['IMPORT', 'MANUAL', 'LEAVE_SYNC']);
  for (const code of SOURCE_VALUES) {
    const branch = new RegExp(`SOURCE_${code}\\s*=>\\s*"([^"]+)"`).exec(RUST_RECORD_DTO);
    assert.ok(branch, `后端 source_label 缺少 ${code} 分支`);
    assert.equal(SOURCE_LABELS[code], branch[1], `${code} 的中文名与后端不一致`);
  }
  const check = /CHECK \(source IN \(([^)]+)\)\)/.exec(SQL_HR_TABLES);
  assert.ok(check, 'V0383 的 source CHECK 没找到');
  for (const code of SOURCE_VALUES) {
    assert.ok(check[1].includes(`'${code}'`), `库层 CHECK 缺 ${code}`);
  }
});

/** 未知码值回原值（排障时看到 `NEW_STATE` 比看到「未知」有用） */
test('unknown codes fall back to the raw code', () => {
  assert.equal(periodStatusLabel('NEW_STATE'), 'NEW_STATE');
  assert.equal(periodStatusLabel(''), '-');
  assert.equal(sourceLabel('EDGE_CASE'), 'EDGE_CASE');
  assert.equal(periodStatusTheme('NOPE'), 'default');
  assert.equal(periodStatusTheme('LOCKED'), 'danger');
});

// ==================== ② 状态机同域 ====================

/** 三条边：`DRAFT→SUBMITTED` / `SUBMITTED→LOCKED` / `LOCKED→DRAFT` */
test('period actions mirror ATTENDANCE_PERIOD_EDGES', () => {
  const edges = /pub const ATTENDANCE_PERIOD_EDGES: \[\(&str, &str\); (\d+)\] =\s*\[([^\]]+)\]/.exec(
    RUST_STATE_MACHINE,
  );
  assert.ok(edges, 'state_machine 里找不到 ATTENDANCE_PERIOD_EDGES');
  const count = Number(edges[1]);
  const pairs = [...edges[2].matchAll(/\("(\w+)",\s*"(\w+)"\)/g)].map((m) => [m[1], m[2]]);
  assert.equal(pairs.length, count, '边表条数与声明不符');
  assert.deepEqual(pairs, [
    ['DRAFT', 'SUBMITTED'],
    ['SUBMITTED', 'LOCKED'],
    ['LOCKED', 'DRAFT'],
  ]);

  const fromKeys = pairs.map((pair) => pair[0]);
  for (const status of PERIOD_STATUS_VALUES) {
    const actions = periodActionsOf(status);
    assert.equal(actions.submit, fromKeys.includes(status) && pairs.some((p) => p[0] === status && p[1] === 'SUBMITTED'));
    assert.equal(actions.lock, pairs.some((p) => p[0] === status && p[1] === 'LOCKED'), `${status} 的锁定态不对`);
    assert.equal(actions.unlock, pairs.some((p) => p[0] === status && p[1] === 'DRAFT'), `${status} 的解锁态不对`);
    // 锁定 / 已锁定的期间一律不能改 / 删 / 重算（后端 update_period / recalc / delete 只拦 LOCKED）
    assert.equal(actions.edit, status !== 'LOCKED');
    assert.equal(actions.recalc, status !== 'LOCKED');
    assert.equal(actions.remove, status === 'DRAFT');
    // 锁定期台账必须能看（后端模块文档：锁定期间的台账正是发工资前该核对的那份）
    assert.equal(actions.ledger, true);
  }
  // 草稿上不能出现「锁定」按钮（要先把期间提交）
  assert.equal(periodActionsOf('DRAFT').lock, false);
});

/** 台账下拉默认只列 DRAFT / SUBMITTED（设计 §6.2.3） */
test('ledger period dropdown defaults to unlocked periods', () => {
  assert.deepEqual([...LEDGER_PERIOD_STATUSES], ['DRAFT', 'SUBMITTED']);
  assert.ok(!LEDGER_PERIOD_STATUSES.includes('LOCKED'), '锁定期间默认不进下拉（「含已锁定」开关才加）');
  assert.ok(PAGE.includes('includeLocked'), '页面缺少「含已锁定」开关');
  assert.ok(PAGE.includes("'LOCKED'"), '「含已锁定」没有把 LOCKED 拼进查询');
});

// ==================== ③ 上限 / 常量对齐 ====================

test('limits mirror backend DTO constants', () => {
  const span = /pub const MAX_SPAN_DAYS: i64 = (\d+);/.exec(RUST_PERIOD_DTO);
  assert.ok(span, '后端 MAX_SPAN_DAYS 没找到');
  assert.equal(MAX_SPAN_DAYS, Number(span[1]));

  const reason = /pub const MIN_REASON_CHARS: usize = (\d+);/.exec(RUST_PERIOD_DTO);
  assert.ok(reason, '后端 MIN_REASON_CHARS 没找到');
  assert.equal(MIN_UNLOCK_REASON, Number(reason[1]));

  const periodCodeLen = /pub const PERIOD_CODE_LEN: usize = (\d+);/.exec(RUST_PERIOD_DTO);
  assert.ok(periodCodeLen, '后端 PERIOD_CODE_LEN 没找到');
  assert.equal(PERIOD_CODE_LEN, Number(periodCodeLen[1]));

  const remark = /pub const MAX_REMARK: usize = (\d+);/.exec(RUST_PERIOD_DTO);
  assert.ok(remark, '后端期间 MAX_REMARK 没找到');
  assert.equal(PERIOD_MAX_REMARK, Number(remark[1]));

  // 天数 31 / 小时 744 / 次数 62：后端 `max_days()` / `max_ot_hours()` / `MAX_COUNT`
  assert.ok(RUST_RECORD_DTO.includes('Decimal::new(31, 0)'), '后端天数上限不是 31');
  assert.ok(RUST_RECORD_DTO.includes('Decimal::new(744, 0)'), '后端小时上限不是 744');
  const maxCount = /pub const MAX_COUNT: i32 = (\d+);/.exec(RUST_RECORD_DTO);
  assert.ok(maxCount, '后端 MAX_COUNT 没找到');
  const dayFields = NUMBER_FIELDS.filter((field) => field.unit === '天');
  const hourFields = NUMBER_FIELDS.filter((field) => field.unit === '小时');
  const countFields = NUMBER_FIELDS.filter((field) => field.unit === '次');
  assert.equal(dayFields.length, 6, '天数类列应是 6 个（应出勤/实际/带薪假/无薪假/病假/旷工）');
  assert.equal(hourFields.length, 3, '加班小时应是 3 列');
  assert.equal(countFields.length, 2, '迟到早退应是 2 列');
  for (const field of dayFields) {
    assert.equal(field.max, 31, `${field.key} 天数上限应为 31`);
  }
  for (const field of hourFields) {
    assert.equal(field.max, 744, `${field.key} 小时上限应为 744`);
  }
  for (const field of countFields) {
    assert.equal(field.max, Number(maxCount[1]), `${field.key} 次数上限应与后端一致`);
    assert.equal(field.integer, true, `${field.key} 是 INT 列，出参是数字`);
  }
  // 11 个可写数值列（后端 `RecordNumbers`）
  assert.equal(NUMBER_FIELDS.length, 11, '可编辑数值列必须是 11 个');
});

/** 权限码与 `V0387__hr_menu_perms` 逐字一致 */
test('permission codes are seeded by V0387', () => {
  for (const code of Object.values(ATTENDANCE_PERMS)) {
    assert.ok(SQL_HR_MENU.includes(`'${code}'`), `菜单种子缺权限码 ${code}`);
  }
  assert.equal(ATTENDANCE_PERMS.list, 'business:hr:attendance:list');
  assert.equal(ATTENDANCE_PERMS.import, 'business:hr:attendance:import');
  assert.equal(ATTENDANCE_PERMS.lock, 'business:hr:attendance:lock');
});

/** 表格列：前 4 列 + 11 个数值列 + 尾部（列序 = 设计 §6.2.3） */
test('record columns cover every editable field exactly once', () => {
  const keys = ATTENDANCE_RECORD_COLUMNS.map((column) => column.colKey);
  for (const field of NUMBER_FIELDS) {
    const hits = keys.filter((key) => key === field.key);
    assert.equal(hits.length, 1, `${field.key} 在台账列里出现 ${hits.length} 次`);
  }
  assert.deepEqual(keys.slice(0, 3), ['employee_no', 'employee_name', 'dept_name']);
  assert.ok(keys.includes('ot_hours_total'), '缺「加班合计」列');
  const periodKeys = ATTENDANCE_PERIOD_COLUMNS.map((column) => column.colKey);
  assert.ok(periodKeys.includes('range'), '期间列表缺「考勤周期」列');
  assert.ok(periodKeys.includes('op'), '期间列表缺操作列');
});

// ==================== ④ 数值口径 ====================

test('decimalText drops trailing zeros without floating point', () => {
  assert.equal(decimalText('22.00'), '22');
  assert.equal(decimalText('7.50'), '7.5');
  assert.equal(decimalText('0.00'), '0');
  assert.equal(decimalText('744'), '744');
  assert.equal(decimalText(''), '0');
  assert.equal(decimalText(null), '0');
  // 后端出参异常时原样显示（宁可显示原文，也不要显示 NaN）
  assert.equal(decimalText('abc'), 'abc');
  // 小数位保留（不能把 0.05 变成 0.1）
  assert.equal(decimalText('0.05'), '0.05');
});

test('decimalValue feeds t-input-number with trimmed numbers', () => {
  assert.equal(decimalValue('22.00'), 22);
  assert.equal(decimalValue('7.25'), 7.25);
  assert.equal(decimalValue('7.259', 2), 7.26, '按列小数位截断');
  assert.equal(decimalValue(''), 0);
  assert.equal(decimalValue('abc'), 0);
});

test('cellText keeps INT columns integral', () => {
  const row = sampleRecord();
  assert.equal(cellText(row, 'late_count'), '1');
  assert.equal(cellText(row, 'should_days'), '22');
  assert.equal(cellText(row, 'ot_hours_workday'), '10');
  assert.equal(cellText(row, 'sick_days'), '0');
});

// ==================== ⑤ 期间展示 / 筛选 ====================

test('period labels carry status so locked vs editable is visible', () => {
  const row = samplePeriod();
  assert.equal(periodRangeText(row), '2026-10-26 ~ 2026-11-25');
  assert.equal(periodOptionLabel(row), '2026-11（10-26 ~ 11-25）· 已提交');
  const options = periodOptions([row]);
  assert.deepEqual(options, [{ label: '2026-11（10-26 ~ 11-25）· 已提交', value: row.id }]);
  assert.match(periodAuditText({ ...row, submitted_by_name: '张三', submitted_at: '2026-11-02T09:10:00+08:00', locked_at: null }), /提交 张三 2026-11-02 09:10/);
  assert.match(periodAuditText(row), /锁定 —/);
});

test('datetimeText only slices the string (no timezone shift)', () => {
  assert.equal(datetimeText('2026-11-02T09:10:00+08:00'), '2026-11-02 09:10');
  assert.equal(datetimeText('2026-11-02 09:10:11'), '2026-11-02 09:10');
  assert.equal(datetimeText(''), '');
  assert.equal(datetimeText('not-a-date'), 'not-a-date');
  assert.equal(textOr(''), '-');
  assert.equal(textOr(' 备注 '), '备注');
});

test('period query serialises multi-select as comma string', () => {
  const filter = emptyPeriodFilter();
  assert.deepEqual(buildPeriodQuery(filter, 1, 20), { page: 1, page_size: 20 });
  filter.keyword = ' 2026-11 ';
  filter.status = ['DRAFT', 'LOCKED'];
  filter.year = '2026';
  assert.deepEqual(buildPeriodQuery(filter, 2, 20), {
    page: 2,
    page_size: 20,
    keyword: '2026-11',
    status: 'DRAFT,LOCKED',
    year: 2026,
  });
  // page_size 夹在 1..200（挡住 page_size=100000 把列表变导出）
  assert.equal(buildPeriodQuery(filter, 0, 100000).page_size, 200);
  assert.equal(buildPeriodQuery(filter, 0, 0).page_size, 1);
  assert.equal(buildPeriodQuery(filter, -3, 20).page, 1);
  assert.equal(isPeriodFiltered(emptyPeriodFilter()), false);
  assert.equal(isPeriodFiltered({ keyword: ' ', status: [], year: '' }), false);
  assert.equal(isPeriodFiltered({ keyword: '', status: ['DRAFT'], year: '' }), true);
});

test('record query refuses to fire without period_id', () => {
  assert.equal(buildRecordQuery(emptyRecordFilter(), 1, 20), null, '没选期间就不该打请求');
  const filter: ReturnType<typeof emptyRecordFilter> = { period_id: 'p-1', dept_id: ' d-1 ', keyword: ' 张三 ' };
  assert.deepEqual(buildRecordQuery(filter, 1, 20), {
    period_id: 'p-1',
    page: 1,
    page_size: 20,
    dept_id: 'd-1',
    keyword: '张三',
  });
  assert.equal(isRecordFiltered(emptyRecordFilter()), false);
  assert.equal(isRecordFiltered({ period_id: 'p-1', dept_id: '', keyword: 'x' }), true);
});

// ==================== ⑥ 期间表单 ====================

test('emptyPeriodForm defaults to the current natural month', () => {
  const form = emptyPeriodForm(new Date(2026, 1, 15));
  assert.equal(form.period_code, '2026-02');
  assert.equal(form.period_start, '2026-02-01');
  assert.equal(form.period_end, '2026-02-28', '平年 2 月是 28 天');
  assert.equal(emptyPeriodForm(new Date(2026, 10, 15)).period_end, '2026-11-30');
});

test('period code / date validators reject what the backend rejects', () => {
  for (const ok of ['2026-11', '2026-01', '2026-12']) {
    assert.equal(isPeriodCode(ok), true, `${ok} 应合法`);
  }
  for (const bad of ['2026-13', '2026-00', '2026-1', '26-11', '2026/11', '2026-11-01', '']) {
    assert.equal(isPeriodCode(bad), false, `${bad} 应非法（后端 check_period_code 同口径）`);
  }
  for (const ok of ['2026-02-28', '2024-02-29', '2026-12-31']) {
    assert.equal(isDateText(ok), true, `${ok} 应合法`);
  }
  for (const bad of ['2026-02-30', '2026-13-01', '2026-1-1', '']) {
    assert.equal(isDateText(bad), false, `${bad} 应非法`);
  }
  assert.equal(spanDaysOf('2026-11-01', '2026-11-30'), 30);
  assert.equal(spanDaysOf('2026-10-26', '2026-11-25'), 31);
  assert.equal(spanDaysOf('2026-11-30', '2026-11-01'), 0);
});

test('validatePeriodForm matches parse_create rules', () => {
  const base = emptyPeriodForm(new Date(2026, 10, 15));
  assert.deepEqual(validatePeriodForm(base), []);

  const badCode = validatePeriodForm({ ...base, period_code: '2026-13' });
  assert.equal(badCode[0].field, 'period_code');
  assert.match(badCode[0].message, /01~12/);

  const reversed = validatePeriodForm({ ...base, period_end: '2026-10-01' });
  assert.equal(reversed[0].field, 'period_end');
  assert.match(reversed[0].message, /不能早于/);

  const tooLong = validatePeriodForm({ ...base, period_end: '2026-12-31' });
  assert.match(tooLong[0].message, /跨度/, '跨两月必须拦（后端 MAX_SPAN_DAYS=31）');

  const badDate = validatePeriodForm({ ...base, period_start: '2026-02-30' });
  assert.equal(badDate[0].field, 'period_start');

  const longRemark = validatePeriodForm({ ...base, remark: '备'.repeat(PERIOD_MAX_REMARK + 1) });
  assert.equal(longRemark[0].field, 'remark');

  // 26 日 ~ 次月 25 日的计薪周期必须放行（设计 §3.1 ④）
  assert.deepEqual(validatePeriodForm({ ...base, period_code: '2026-11', period_start: '2026-10-26', period_end: '2026-11-25' }), []);
});

test('create / update payloads follow the backend contract', () => {
  const form = emptyPeriodForm(new Date(2026, 10, 15));
  form.remark = '  按 26-25 计薪  ';
  assert.deepEqual(toPeriodCreatePayload(form), {
    period_code: '2026-11',
    period_start: '2026-11-01',
    period_end: '2026-11-30',
    remark: '按 26-25 计薪',
  });
  const update = toPeriodUpdatePayload(form);
  assert.deepEqual(update, {
    period_start: '2026-11-01',
    period_end: '2026-11-30',
    remark: '按 26-25 计薪',
  });
  assert.ok(!('period_code' in update), 'period_code 不在可改列里（后端不收）');
  // 备注显式空串 = 清空（后端 double_option 区分「缺省」与「清空」）
  assert.equal(toPeriodUpdatePayload({ ...form, remark: '   ' }).remark, '');

  const row = samplePeriod();
  assert.deepEqual(periodFormFromRow(row), {
    period_code: '2026-11',
    period_start: '2026-10-26',
    period_end: '2026-11-25',
    remark: '',
  });
});

test('status change payload only carries a reason on unlock', () => {
  assert.deepEqual(statusChangePayload('SUBMITTED', '随便写'), { to_status: 'SUBMITTED' });
  assert.deepEqual(statusChangePayload('LOCKED', '随便写'), { to_status: 'LOCKED' });
  assert.deepEqual(statusChangePayload('DRAFT', '  薪资口径调整，需重导  '), {
    to_status: 'DRAFT',
    reason: '薪资口径调整，需重导',
  });
  assert.match(validateUnlockReason(''), /解锁必须填原因/);
  assert.match(validateUnlockReason('手滑'), /至少 5 个字/);
  assert.equal(validateUnlockReason('薪资口径调整'), '');
  // 5 字边界（后端 MIN_REASON_CHARS）
  assert.equal(validateUnlockReason('一二三四五'), '');
});

// ==================== ⑦ 单元格失焦保存 ====================

test('recordPatchOf sends exactly one field and skips no-op edits', () => {
  const row = sampleRecord();
  assert.equal(recordPatchOf(row, 'should_days', 22), null, '同值（22 vs 22.00）不发请求');
  assert.equal(recordPatchOf(row, 'late_count', 1), null, '同值次数不发请求');

  assert.deepEqual(recordPatchOf(row, 'actual_days', 19.5), { actual_days: '19.5' });
  // 字符串入参（t-input-number 有时回字符串）
  assert.deepEqual(recordPatchOf(row, 'leave_unpaid_days', '2.5'), { leave_unpaid_days: '2.5' });
  // 次数列发数字（库列是 INT，发字符串后端也能收，但口径上应发数字）
  const counts = recordPatchOf(row, 'late_count', 2);
  assert.deepEqual(counts, { late_count: 2 });
  assert.equal(typeof (counts as { late_count: number }).late_count, 'number');
  // 清空（t-input-number 回 null）按 0 处理，而不是发 null 让后端当成「不改」
  assert.deepEqual(recordPatchOf(row, 'sick_days', null), null, '0 == 0 时无变化');
  const rowWithSick = { ...row, sick_days: '1.00' };
  assert.deepEqual(recordPatchOf(rowWithSick, 'sick_days', ''), { sick_days: '0' });
  // 负数截到 0（后端 validate 会 400，前端先不让它在框里出现）
  assert.deepEqual(recordPatchOf(row, 'late_count', -3), { late_count: 0 });
  // 小数位多的输入按字符串原样保留（后端 Decimal 精确解析）
  assert.deepEqual(recordPatchOf(row, 'ot_hours_holiday', '1.25'), { ot_hours_holiday: '1.25' });
});

test('totals helpers render the period snapshot', () => {
  const row = samplePeriod();
  const totals = totalsOfPeriod(row);
  assert.deepEqual(totals, {
    employee_count: 2,
    should_days_total: '44.00',
    actual_days_total: '40.00',
    ot_hours_total: '28.00',
  });
  assert.equal(totalsText(totals), '共 2 人 · 应出勤 44 天 · 实际 40 天 · 加班 28 小时');
  assert.equal(totalsText(emptyTotals()), '共 0 人 · 应出勤 0 天 · 实际 0 天 · 加班 0 小时');
});

test('numberFieldMeta falls back instead of throwing', () => {
  assert.equal(numberFieldMeta('should_days').label, '应出勤');
  assert.equal(numberFieldMeta('absent_days').unit, '天');
  assert.equal(numberFieldMeta('nope' as never).key, 'should_days');
});

// ==================== ⑧ 接线（页面 / API / 路由） ====================

test('page wires the real endpoints, not stubs', () => {
  // 期间切换：下拉 + change 重查
  assert.ok(PAGE.includes('v-model="recordFilter.period_id"'), '台账期间下拉没绑 period_id');
  assert.ok(PAGE.includes('@change="handlePeriodSwitch"'), '期间切换没有重查');
  // 单元格编辑：动态列 + 失焦保存 + 只发一列
  assert.ok(PAGE.includes('@blur="handleCellBlur(row, field)"'), '单元格没有失焦保存');
  assert.ok(PAGE.includes('recordPatchOf(row, field.key, cellDraft[key])'), '失焦没有走 recordPatchOf');
  assert.ok(PAGE.includes('await patchRecord(row.id, payload)'), '没有真的调单行修正接口');
  assert.ok(PAGE.includes(':disabled="!row.editable || !canRecord"'), '锁定期间的单元格没禁用');
  // 导入：模板 + 上传 + 预检 + 确认
  assert.ok(PAGE.includes('<ImportEntryButton'), '缺导入入口组件');
  assert.ok(PAGE.includes(':biz-type="ATTENDANCE_BIZ_TYPE"'), '导入入口没带 biz-type');
  assert.ok(PAGE.includes('await uploadAttendanceImport(importFile.value)'), '没有真的上传');
  assert.ok(PAGE.includes('await confirmAttendanceImport(task.task_id)'), '没有确认入库');
  // 期间维护 + 状态流转
  assert.ok(PAGE.includes('await createPeriod('), '没有建期间');
  assert.ok(PAGE.includes('await updatePeriod('), '没有改期间');
  assert.ok(PAGE.includes('await deletePeriod('), '没有删期间');
  assert.ok(PAGE.includes('await changePeriodStatus('), '没有状态流转');
  assert.ok(PAGE.includes('await recalcPeriod('), '没有重算');
  // 错误回滚：失败时把输入框退回库里的数
  assert.ok(PAGE.includes('cellDraft[key] = decimalValue(row[field.key], field.decimals)'), '保存失败没有回滚');
  // 汇总口径：修正 / 重算 / 导入后的数字要能撑过「切走再切回来」（否则合计会回落到旧值）
  assert.ok(PAGE.includes('totalsCache[result.record.period_id] = result.period_totals'), '修正回执的合计没进缓存');
  assert.ok(PAGE.includes('totalsCache[periodId] = recalc.after'), '导入后的重算结果没进缓存');
  assert.ok(PAGE.includes('const recalc = await recalcPeriod(periodId)'), '导入后没有补重算（通用上传路径不重算期间头）');
});

test('api layer points at the real backend paths with /api prefix', () => {
  for (const path of [
    '/attendance/period/page',
    '/attendance/period',
    '/attendance/period/${id}',
    '/attendance/period/${id}/status',
    '/attendance/period/${id}/recalc',
    '/attendance/record/page',
    '/attendance/record/${id}',
  ]) {
    assert.ok(API.includes(path), `API 层缺路径 ${path}`);
  }
  assert.ok(API.includes("const BASE = '/api/business/hr'"), 'BASE 必须带 /api 前缀');
  assert.equal(ATTENDANCE_BIZ_TYPE, 'hr.attendance');
  // 导入走 impexp 通用三段（HR 专属路由未落地，见 attendance.ts 文件头）
  assert.ok(API.includes('uploadImportFile(file, ATTENDANCE_BIZ_TYPE'), '导入没有固定 biz_type');
  assert.ok(API.includes('confirmImport(taskId)'), '确认入库没接');
});

test('router registers the attendance page', () => {
  assert.ok(ROUTER.includes("path: 'business/hr/attendance'"), '考勤路由没注册');
  assert.ok(ROUTER.includes('business/hr/attendance/index.vue'), '路由组件路径不对');
  assert.ok(ROUTER.includes("permission: 'business:hr:attendance:list'"), '路由权限码不对');
});

// ==================== 样例 ====================

function samplePeriod(): AttendancePeriodRow {
  return {
    id: 'period-1',
    period_code: '2026-11',
    period_start: '2026-10-26',
    period_end: '2026-11-25',
    status: 'SUBMITTED',
    status_label: '已提交',
    locked: false,
    employee_count: 2,
    should_days_total: '44.00',
    actual_days_total: '40.00',
    ot_hours_total: '28.00',
    import_task_id: null,
    submitted_at: '2026-11-01T10:00:00+08:00',
    submitted_by: null,
    submitted_by_name: null,
    locked_at: null,
    locked_by: null,
    locked_by_name: null,
    remark: null,
    created_at: '2026-10-26T09:00:00+08:00',
    updated_at: '2026-11-01T10:00:00+08:00',
  };
}

function sampleRecord(): AttendanceRecordRow {
  return {
    id: 'record-1',
    period_id: 'period-1',
    period_code: '2026-11',
    period_status: 'SUBMITTED',
    period_status_label: '已提交',
    editable: true,
    employee_id: 'emp-1',
    employee_no: 'EMP-0001',
    employee_name: '张三',
    dept_id: 'dept-1',
    dept_name: '生产部',
    should_days: '22.00',
    actual_days: '20.00',
    leave_paid_days: '0.00',
    leave_unpaid_days: '2.00',
    sick_days: '0.00',
    absent_days: '0.00',
    ot_hours_workday: '10.00',
    ot_hours_weekend: '4.00',
    ot_hours_holiday: '0.00',
    late_count: 1,
    early_leave_count: 0,
    ot_hours_total: '14.00',
    source: 'IMPORT',
    source_label: '导入',
    remark: null,
    updated_at: '2026-11-02T09:10:00+08:00',
    updated_by: null,
    updated_by_name: null,
  };
}
