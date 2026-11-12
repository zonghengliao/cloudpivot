/**
 * 请假单页（HR-28）单测：全部跑在无 DOM 的 `tsx --test` 里。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * # 测什么
 *
 * 1. **词表镜像**：`leave-kinds.ts` 的 4 张「码值 → 中文」表（假别 9 / 状态 4 / 带薪 2 /
 *    审批待办 7）逐条对齐后端 `backend/src/business/hr/leave_dto.rs`（直接读 Rust 源码）。
 *    后端改取值而前端没跟 → 这里先红，而不是线上看到 `PERSONAL` 这种码值。
 * 2. **口径**：`mine` 只在「我的请假」tab 出现（漏传 = 看到全员数据）；多选按逗号串
 *    （后端 `serde_urlencoded` 不收重复键）；`duration_days` 去尾零；`ISO → 本地展示`
 *    只切字符串不转时区；动作态按状态推（与后端 `EDITABLE_STATUSES` /
 *    `CANCELLABLE_STATUSES` / `LeaveCancelAction::of_status` 同域）。
 * 3. **表单校验**：与后端 `create_of` 的判据逐条对齐（0.5 步长 / 天数 ≤ 起止跨度 /
 *    三条作废路都要 ≥5 字原因）。
 * 4. **接线**：`index.vue` 是否真的用了这些纯函数、是否真的把 `mine` 传下去
 *    （逻辑对、模板写错也要红）。
 *
 * 不渲染 `.vue`：本仓没有 happy-dom/jsdom（零新依赖），渲染层用源码断言兜住。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

import {
  APPROVAL_TASK_LABELS,
  LEAVE_CANCELLABLE_STATUSES,
  LEAVE_COLUMNS,
  LEAVE_EDITABLE_STATUSES,
  LEAVE_LOCKED_STATUSES,
  LEAVE_MAX_CANCEL_REASON,
  LEAVE_MIN_CANCEL_REASON,
  LEAVE_STATUS_LABELS,
  LEAVE_STATUS_VALUES,
  LEAVE_SUBMITTABLE_STATUSES,
  LEAVE_TABS,
  LEAVE_TYPE_LABELS,
  LEAVE_TYPE_VALUES,
  LEAVE_WITHDRAWABLE_STATUSES,
  PAID_LABELS,
  approvalTaskTheme,
  buildLeaveQuery,
  cancelActionOf,
  cancelActionLabel,
  daySpanOf,
  datetimeText,
  defaultLeaveTab,
  durationText,
  emptyLeaveFilter,
  emptyLeaveForm,
  formFromRow,
  isHalfStep,
  isLeaveFiltered,
  leaveActionsOf,
  leaveStatusTheme,
  leaveTypeLabel,
  minuteOfDay,
  paidLabel,
  rangeText,
  suggestDuration,
  tabIsMine,
  textOf,
  toCreatePayload,
  toUpdatePayload,
  validateCancelReason,
  validateLeaveForm,
  type LeaveListRow,
} from '@/api/business/hr/leave-kinds';

/** 后端契约源码（词表 + 状态清单的唯一真源） */
const RUST_DTO = readFileSync(
  new URL('../../../backend/src/business/hr/leave_dto.rs', import.meta.url),
  'utf8',
);

/** 页面源码（接线断言用） */
const PAGE = readFileSync(
  new URL('../../src/views/business/hr/leave/index.vue', import.meta.url),
  'utf8',
);

// ==================== ① 词表镜像 ====================

test('假别 9 值与后端 LEAVE_TYPES 逐值保序一致', () => {
  const block = /pub const LEAVE_TYPES: \[&str; 9\] = \[([\s\S]*?)\];/.exec(RUST_DTO);
  assert.ok(block, '后端 LEAVE_TYPES 常量没找到（契约改了？）');
  const rustTypes = [...block[1].matchAll(/LEAVE_[A-Z]+\s*,/g)].map((item) => item[0].replace(/[\s,]/g, ''));
  assert.equal(rustTypes.length, 9);

  // 把 Rust 常量名映射回码值：`LEAVE_ANNUAL` → `ANNUAL`
  const codes = rustTypes.map((name) => name.replace(/^LEAVE_/, '').replace(/_/g, ''));
  const backendOrder = ['ANNUAL', 'SICK', 'PERSONAL', 'MARRIAGE', 'MATERNITY', 'PATERNITY', 'BEREAVEMENT', 'COMPENSATORY', 'OTHER'];
  assert.deepEqual(codes, backendOrder, 'Rust 常量顺序与后端声明顺序不一致');
  assert.deepEqual([...LEAVE_TYPE_VALUES], backendOrder, '前端假别顺序与后端不一致');
});

test('假别中文名与后端 leave_type_label 逐条一致', () => {
  const block = /pub fn leave_type_label\(leave_type: &str\) -> &'static str \{([\s\S]*?)\n\}/.exec(RUST_DTO);
  assert.ok(block, '后端 leave_type_label 没找到');
  for (const [code, label] of Object.entries(LEAVE_TYPE_LABELS)) {
    assert.ok(
      block[1].includes(`LEAVE_${code} => "${label}"`),
      `假别 ${code} 的中文名「${label}」与后端不一致`,
    );
  }
  // 反向：后端每个分支前端都要有
  const backendPairs = [...block[1].matchAll(/LEAVE_([A-Z]+) => "([^"]+)"/g)];
  assert.equal(backendPairs.length, Object.keys(LEAVE_TYPE_LABELS).length, '假别条数不一致');
});

test('状态 4 值与中文名与后端逐条一致', () => {
  assert.deepEqual([...LEAVE_STATUS_VALUES], ['DRAFT', 'SUBMITTED', 'APPROVED', 'CANCELLED']);
  const block = /pub fn status_label\(status: &str\) -> &'static str \{([\s\S]*?)\n\}/.exec(RUST_DTO);
  assert.ok(block, '后端 status_label 没找到');
  for (const [code, label] of Object.entries(LEAVE_STATUS_LABELS)) {
    assert.ok(block[1].includes(`STATUS_${code} => "${label}"`), `状态 ${code} 的中文名「${label}」与后端不一致`);
  }
});

test('带薪 2 值与中文名与后端逐条一致', () => {
  assert.deepEqual(PAID_LABELS, { 1: '带薪', 0: '无薪' });
  assert.ok(RUST_DTO.includes('PAID_YES => "带薪"'));
  assert.ok(RUST_DTO.includes('PAID_NO => "无薪"'));
});

test('状态清单（可编辑 / 可提交 / 可撤回 / 可作废 / 锁死）与后端常量逐值一致', () => {
  assert.deepEqual([...LEAVE_EDITABLE_STATUSES], ['DRAFT']);
  assert.ok(RUST_DTO.includes('pub const EDITABLE_STATUSES: [&str; 1] = [STATUS_DRAFT];'));
  assert.deepEqual([...LEAVE_SUBMITTABLE_STATUSES], ['DRAFT']);
  assert.ok(RUST_DTO.includes('pub const SUBMITTABLE_STATUSES: [&str; 1] = [STATUS_DRAFT];'));
  assert.deepEqual([...LEAVE_WITHDRAWABLE_STATUSES], ['SUBMITTED']);
  assert.ok(RUST_DTO.includes('pub const WITHDRAWABLE_STATUSES: [&str; 1] = [STATUS_SUBMITTED];'));
  assert.deepEqual([...LEAVE_CANCELLABLE_STATUSES], ['SUBMITTED', 'APPROVED']);
  assert.ok(
    RUST_DTO.includes('pub const CANCELLABLE_STATUSES: [&str; 2] = [STATUS_SUBMITTED, STATUS_APPROVED];'),
    '可作废清单与后端不一致（草稿走软删，不在这个清单里）',
  );
  // 锁死清单在 state_machine 里（APPROVED / CANCELLED）
  const stateMachine = readFileSync(
    new URL('../../../backend/src/business/shared/state_machine.rs', import.meta.url),
    'utf8',
  );
  const locked = /pub const LEAVE_LOCKED: \[&str; \d+\] = \[([^\]]+)\]/.exec(stateMachine);
  assert.ok(locked, '后端 LEAVE_LOCKED 没找到');
  // 该常量是字面量 `["APPROVED", "CANCELLED"]`（不是 STATUS_* 常量），按引号取值
  const lockedCodes = [...locked[1].matchAll(/"([A-Z]+)"/g)].map((item) => item[1]);
  assert.deepEqual([...LEAVE_LOCKED_STATUSES], lockedCodes, '锁死态清单与后端 state_machine 不一致');
});

test('审批待办 7 个状态与后端 task_status_label 逐条一致', () => {
  const handler = readFileSync(
    new URL('../../../backend/src/business/hr/leave_handler.rs', import.meta.url),
    'utf8',
  );
  for (const [code, label] of Object.entries(APPROVAL_TASK_LABELS)) {
    assert.ok(handler.includes(`"${code}" => "${label}".to_string()`), `待办状态 ${code} 的「${label}」与后端不一致`);
  }
});

// ==================== ② 筛选序列化 ====================

test('「我的请假」tab 才带 mine=true（漏传 = 看到全员数据）', () => {
  const filter = emptyLeaveFilter();
  const all = buildLeaveQuery(filter, 1, 20, false);
  assert.equal(all.mine, undefined, '「全部请假」不能带 mine');
  const mine = buildLeaveQuery(filter, 1, 20, true);
  assert.equal(mine.mine, true, '「我的请假」必须带 mine=true');

  assert.equal(tabIsMine('mine'), true);
  assert.equal(tabIsMine('all'), false);
  assert.deepEqual([...LEAVE_TABS], ['all', 'mine']);
});

test('空筛选不产生空参数（status=[] 会变成 status= 导致后端 400）', () => {
  const query = buildLeaveQuery(emptyLeaveFilter(), 1, 20, false);
  assert.deepEqual(query, { page: 1, page_size: 20 });
});

test('多选按逗号串序列化，日期与关键字原样带上', () => {
  const query = buildLeaveQuery(
    {
      keyword: '  张三 ',
      dept_id: 'dept-1',
      leave_type: ['ANNUAL', 'SICK'],
      status: ['SUBMITTED', 'APPROVED'],
      date_from: '2026-11-01',
      date_to: '2026-11-30',
    },
    3,
    50,
  );
  assert.equal(query.keyword, '张三');
  assert.equal(query.dept_id, 'dept-1');
  assert.equal(query.leave_type, 'ANNUAL,SICK');
  assert.equal(query.status, 'SUBMITTED,APPROVED');
  assert.equal(query.date_from, '2026-11-01');
  assert.equal(query.date_to, '2026-11-30');
  assert.equal(query.page, 3);
  assert.equal(query.page_size, 50);
});

test('页码容错：0 / 负数回第 1 页，pageSize 容错回默认', () => {
  assert.equal(buildLeaveQuery(emptyLeaveFilter(), 0, 0).page, 1);
  assert.equal(buildLeaveQuery(emptyLeaveFilter(), -5, -1).page_size, 20);
});

test('isLeaveFiltered 认得出每一项筛选', () => {
  assert.equal(isLeaveFiltered(emptyLeaveFilter()), false);
  assert.equal(isLeaveFiltered({ ...emptyLeaveFilter(), keyword: '  ' }), false, '全空格不算筛选');
  for (const patch of [
    { keyword: 'LV' },
    { dept_id: 'd' },
    { leave_type: ['SICK'] },
    { status: ['DRAFT'] },
    { date_from: '2026-11-01' },
    { date_to: '2026-11-30' },
  ]) {
    assert.equal(isLeaveFiltered({ ...emptyLeaveFilter(), ...patch }), true, `漏判：${JSON.stringify(patch)}`);
  }
});

// ==================== ③ 展示口径 ====================

test('datetimeText 只切字符串、不转时区（09:00 的半天假必须显示 09:00）', () => {
  assert.equal(datetimeText('2026-11-10T09:00:00+08:00'), '2026-11-10 09:00');
  assert.equal(datetimeText('2026-11-10 09:00'), '2026-11-10 09:00');
  assert.equal(datetimeText('2026-11-10T23:30:00-05:00'), '2026-11-10 23:30', '按后端给的墙上时间显示');
  assert.equal(datetimeText(''), '');
  assert.equal(datetimeText('不是时间'), '不是时间', '不认识就原样返回，不显示 Invalid Date');
});

test('durationText 去尾零（Decimal 恒带两位小数）', () => {
  assert.equal(durationText('1.50'), '1.5');
  assert.equal(durationText('2.00'), '2');
  assert.equal(durationText('0.50'), '0.5');
  assert.equal(durationText('1'), '1');
  assert.equal(durationText(''), '');
  assert.equal(durationText(null), '');
});

test('rangeText：同一天只显示一个日期，跨天显示两个', () => {
  assert.equal(
    rangeText({ start_time: '2026-11-10T09:00:00+08:00', end_time: '2026-11-10T12:00:00+08:00' }),
    '2026-11-10 09:00 ~ 12:00',
  );
  assert.equal(
    rangeText({ start_time: '2026-11-10T09:00:00+08:00', end_time: '2026-11-12T18:00:00+08:00' }),
    '2026-11-10 09:00 ~ 2026-11-12 18:00',
  );
});

test('label 兜底：未知码值显码值而不是空白', () => {
  assert.equal(leaveTypeLabel('NEW_TYPE'), 'NEW_TYPE');
  assert.equal(leaveTypeLabel('SICK'), '病假');
  assert.equal(paidLabel(1), '带薪');
  assert.equal(paidLabel(9), '9');
  assert.equal(leaveStatusTheme('WHATEVER'), 'default');
  assert.equal(leaveStatusTheme('APPROVED'), 'success');
});

test('daySpanOf 含首尾（后端「天数 ≤ 起止覆盖自然日数」的口径）', () => {
  assert.equal(daySpanOf('2026-11-10 09:00', '2026-11-10 18:00'), 1);
  assert.equal(daySpanOf('2026-11-10 09:00', '2026-11-12 18:00'), 3);
  assert.equal(daySpanOf('2026-11-10 09:00', '2026-12-10 18:00'), 31);
  assert.equal(daySpanOf('', ''), null);
});

test('suggestDuration：同一天上午到中午 = 0.5，跨天中午结束 = 跨度 - 0.5', () => {
  assert.equal(suggestDuration('2026-11-10 09:00', '2026-11-10 12:00'), '0.5');
  assert.equal(suggestDuration('2026-11-10 09:00', '2026-11-10 18:00'), '1');
  assert.equal(suggestDuration('2026-11-10 09:00', '2026-11-12 18:00'), '3');
  assert.equal(suggestDuration('2026-11-10 09:00', '2026-11-12 12:00'), '2.5');
  assert.equal(suggestDuration('', ''), '');
});

test('minuteOfDay 解析时刻', () => {
  assert.equal(minuteOfDay('2026-11-10 12:00'), 720);
  assert.equal(minuteOfDay('2026-11-10T09:30:00+08:00'), 570);
  assert.equal(minuteOfDay('2026-11-10'), null);
});

// ==================== ④ 动作可用性（三岔口） ====================

test('作废接口一条路三件事：草稿删除 / 审批中撤回 / 已批准销假', () => {
  assert.equal(cancelActionOf('DRAFT'), 'DELETED');
  assert.equal(cancelActionOf('SUBMITTED'), 'WITHDRAWN');
  assert.equal(cancelActionOf('APPROVED'), 'CANCELLED');
  assert.equal(cancelActionOf('CANCELLED'), null, '已作废是终态，不能再作废一次');
  assert.equal(cancelActionOf('WHATEVER'), null);
  assert.equal(cancelActionOf(' draft '), 'DELETED', '大小写/空格容错');

  assert.equal(cancelActionLabel('DRAFT'), '删除');
  assert.equal(cancelActionLabel('SUBMITTED'), '撤回');
  assert.equal(cancelActionLabel('APPROVED'), '销假');
});

test('后端 cancel_of 对三条路都要求原因 ≥5 字（前端必须三条都问原因）', () => {
  // 后端在状态分流之前统一校验 reason（leave_dto::cancel_of）
  const block = /pub fn cancel_of\(req: &LeaveCancelReq\)([\s\S]*?)\n\}/.exec(RUST_DTO);
  assert.ok(block, '后端 cancel_of 没找到');
  assert.ok(block[1].includes('MIN_CANCEL_REASON_CHARS'), '后端原因下限常量没在 cancel_of 里');
  assert.equal(LEAVE_MIN_CANCEL_REASON, 5);
  assert.equal(LEAVE_MAX_CANCEL_REASON, 255);
  assert.ok(RUST_DTO.includes('pub const MIN_CANCEL_REASON_CHARS: usize = 5;'));
  assert.ok(RUST_DTO.includes('pub const MAX_CANCEL_REASON: usize = 255;'));

  for (const status of ['DRAFT', 'SUBMITTED', 'APPROVED']) {
    const state = leaveActionsOf(status);
    assert.equal(state.cancel, true, `${status} 应有作废动作`);
    assert.equal(state.cancelNeedsReason, true, `${status} 的作废必须问原因（后端统一校验）`);
  }
});

test('动作态严格按状态：只有草稿能编辑/提交，只有审批中能撤回', () => {
  const draft = leaveActionsOf('DRAFT');
  assert.deepEqual(
    { edit: draft.edit, submit: draft.submit, cancel: draft.cancel },
    { edit: true, submit: true, cancel: true },
  );

  const submitted = leaveActionsOf('SUBMITTED');
  assert.deepEqual(
    { edit: submitted.edit, submit: submitted.submit, cancel: submitted.cancel },
    { edit: false, submit: false, cancel: true },
  );
  assert.equal(submitted.cancelAction, 'WITHDRAWN');

  const approved = leaveActionsOf('APPROVED');
  assert.deepEqual(
    { edit: approved.edit, submit: approved.submit, cancel: approved.cancel },
    { edit: false, submit: false, cancel: true },
  );
  assert.equal(approved.cancelAction, 'CANCELLED');

  const cancelled = leaveActionsOf('CANCELLED');
  assert.deepEqual(
    { detail: cancelled.detail, edit: cancelled.edit, submit: cancelled.submit, cancel: cancelled.cancel },
    { detail: true, edit: false, submit: false, cancel: false },
  );
});

test('审批节点配色：待审批蓝 / 同意绿 / 驳回红 / 其余中性', () => {
  assert.equal(approvalTaskTheme('PENDING'), 'primary');
  assert.equal(approvalTaskTheme('APPROVED'), 'success');
  assert.equal(approvalTaskTheme('REJECTED'), 'danger');
  assert.equal(approvalTaskTheme('SKIPPED'), 'default');
  assert.equal(approvalTaskTheme(''), 'default');
});

// ==================== ⑤ 表单校验 ====================

/** 一份合法的建单表单（各用例只改一个字段） */
function validForm() {
  return {
    ...emptyLeaveForm('emp-1'),
    leave_type: 'PERSONAL',
    start_time: '2026-11-10 09:00',
    end_time: '2026-11-12 18:00',
    duration_days: '3',
    paid: 0,
    reason: '家中有事',
  };
}

test('合法表单零错误', () => {
  assert.deepEqual(validateLeaveForm(validForm()), []);
});

test('建单必须选员工；改草稿不要求（员工不可改）', () => {
  const form = { ...validForm(), employee_id: '' };
  const errors = validateLeaveForm(form, true);
  assert.ok(errors.some((item) => item.field === 'employee_id'));
  assert.deepEqual(validateLeaveForm(form, false), [], '改草稿时 employee_id 缺失不该拦');
});

test('结束时间必须晚于开始时间', () => {
  const errors = validateLeaveForm({ ...validForm(), end_time: '2026-11-10 09:00' });
  assert.ok(errors.some((item) => item.field === 'end_time'));
});

test('天数 0.5 步长 + 天数 ≤ 起止跨度（后端两条判据都镜像）', () => {
  assert.ok(
    validateLeaveForm({ ...validForm(), duration_days: '1.3' }).some((item) => item.field === 'duration_days'),
    '1.3 不是 0.5 的整数倍',
  );
  assert.ok(isHalfStep(0.5) && isHalfStep(1) && isHalfStep(1.5) && isHalfStep(3));
  assert.equal(isHalfStep(1.3), false);

  // 3 天区间填 5 天 → 超跨度
  const over = validateLeaveForm({ ...validForm(), duration_days: '5' });
  assert.ok(
    over.some((item) => item.field === 'duration_days' && item.message.includes('自然日数')),
    '超跨度必须被拦（否则「请 3 天扣 5 天」直接错工资）',
  );
  // 反向：11-10 单日填 3 天
  const overOneDay = validateLeaveForm({
    ...validForm(),
    end_time: '2026-11-10 18:00',
    duration_days: '3',
  });
  assert.ok(overOneDay.some((item) => item.field === 'duration_days'));
  // 边界：正好等于跨度合法
  assert.deepEqual(
    validateLeaveForm({ ...validForm(), end_time: '2026-11-10 18:00', duration_days: '1' }),
    [],
  );
});

test('天数为 0 / 空 / 非数都会被拦', () => {
  for (const value of ['0', '', 'abc', '-1']) {
    assert.ok(
      validateLeaveForm({ ...validForm(), duration_days: value }).some((item) => item.field === 'duration_days'),
      `duration_days=${JSON.stringify(value)} 应被拦`,
    );
  }
});

test('天数是 number 也不炸（t-input-number 的 v-model 写回 number，真浏览器踩过）', () => {
  // 只用 string 会让 `.trim()` 抛 TypeError → 校验静默失效、点保存毫无反应
  assert.deepEqual(validateLeaveForm({ ...validForm(), duration_days: 3 }), []);
  assert.deepEqual(validateLeaveForm({ ...validForm(), end_time: '2026-11-10 18:00', duration_days: 1 }), []);
  assert.ok(
    validateLeaveForm({ ...validForm(), end_time: '2026-11-10 18:00', duration_days: 3 }).some(
      (item) => item.field === 'duration_days',
    ),
    'number 形态也要判「天数 ≤ 跨度」',
  );
  assert.equal(validateLeaveForm({ ...validForm(), duration_days: 1.3 }).length, 1, 'number 形态也要判 0.5 步长');
  assert.equal(toCreatePayload({ ...validForm(), duration_days: 1.5 }).duration_days, '1.5', 'number → 字符串载荷');
  assert.equal(toUpdatePayload({ ...validForm(), duration_days: 2 }).duration_days, '2');
  assert.equal(textOf(undefined), '');
  assert.equal(textOf('  2.50 '), '2.50');
});

test('带薪只能是 0 / 1（DDL 的 ck_biz_leave_paid）', () => {
  assert.ok(validateLeaveForm({ ...validForm(), paid: 2 }).some((item) => item.field === 'paid'));
});

test('事由 / 备注有长度上限（与后端 MAX_REASON / MAX_REMARK 同值）', () => {
  assert.ok(RUST_DTO.includes('pub const MAX_REASON: usize = 500;'));
  assert.ok(RUST_DTO.includes('pub const MAX_REMARK: usize = 1000;'));
  assert.ok(validateLeaveForm({ ...validForm(), reason: 'a'.repeat(501) }).some((item) => item.field === 'reason'));
  assert.ok(validateLeaveForm({ ...validForm(), remark: 'a'.repeat(1001) }).some((item) => item.field === 'remark'));
});

test('作废原因校验：空 / 太短 / 超长', () => {
  assert.notEqual(validateCancelReason(''), '');
  assert.notEqual(validateCancelReason('   '), '');
  assert.notEqual(validateCancelReason('员工'), '');
  assert.equal(validateCancelReason('员工提前返岗'), '');
  assert.notEqual(validateCancelReason('a'.repeat(256)), '');
});

// ==================== ⑥ 载荷 ====================

test('建单载荷：空文本不收，员工与时间原样带', () => {
  const payload = toCreatePayload({ ...validForm(), remark: '   ' });
  assert.equal(payload.employee_id, 'emp-1');
  assert.equal(payload.start_time, '2026-11-10 09:00');
  assert.equal(payload.duration_days, '3');
  assert.equal(payload.paid, 0);
  assert.equal(payload.reason, '家中有事');
  assert.equal(payload.remark, undefined, '空备注不该变成空串（后端 optional_text 会 trim 成 None，等价但少一个键）');
});

test('改草稿载荷：全量带（空串 = 清空，与后端的部分更新语义配套）', () => {
  const payload = toUpdatePayload({ ...validForm(), reason: '', remark: '' });
  assert.equal(payload.reason, '');
  assert.equal(payload.remark, '');
  assert.equal(payload.leave_type, 'PERSONAL');
});

test('formFromRow：ISO 转成 picker 认的格式，天数原样带', () => {
  const row = {
    id: 'l1',
    leave_no: 'LV-20261110-000001',
    employee_id: 'emp-1',
    employee_no: 'E001',
    employee_name: '张三',
    dept_id: null,
    dept_name: '',
    leave_type: 'SICK',
    leave_type_label: '病假',
    start_time: '2026-11-10T09:00:00+08:00',
    end_time: '2026-11-11T18:00:00+08:00',
    duration_days: '2.00',
    paid: 1,
    paid_label: '带薪',
    status: 'DRAFT',
    status_label: '草稿',
    locked: false,
    reason: '感冒',
    approval_instance_id: null,
    submitted_at: null,
    submitted_by: null,
    submitted_by_name: null,
    cancelled_at: null,
    cancelled_by: null,
    cancelled_by_name: null,
    cancel_reason: null,
    remark: null,
    created_at: '2026-11-10T08:00:00+08:00',
    updated_at: '2026-11-10T08:00:00+08:00',
  } as LeaveListRow;
  const form = formFromRow(row);
  assert.equal(form.start_time, '2026-11-10 09:00');
  assert.equal(form.end_time, '2026-11-11 18:00');
  assert.equal(form.duration_days, '2.00', '原样带，后端收 Decimal');
  assert.equal(form.reason, '感冒');
  assert.equal(form.remark, '', 'null → 空串，避免 textarea 显示 null');
});

// ==================== ⑦ 页面接线 ====================

test('页面用纯函数拼查询，并把 mine 交给 buildLeaveQuery', () => {
  assert.ok(PAGE.includes('buildLeaveQuery('), '列表请求必须走 buildLeaveQuery');
  assert.ok(PAGE.includes('mineTab.value'), '「我的请假」tab 必须把 mine 传下去');
  assert.ok(PAGE.includes('tabIsMine'), 'tab → mine 的换算必须走 tabIsMine');
  assert.ok(PAGE.includes('leaveActionsOf'), '按钮显隐必须由 leaveActionsOf 算，不自己 match 状态');
  assert.ok(PAGE.includes('validateLeaveForm'), '提交前必须走 validateLeaveForm');
  assert.ok(PAGE.includes('validateCancelReason'), '作废前必须校验原因');
  assert.ok(PAGE.includes('durationText('), '天数显示必须去尾零');
  assert.ok(PAGE.includes('rangeText('), '区间显示必须走 rangeText');
  // 两个 tab 都在
  assert.ok(PAGE.includes('value="all"') && PAGE.includes('value="mine"'));
  // 三个接口都要被调到
  for (const fn of ['fetchLeavePage', 'fetchLeaveDetail', 'createLeave', 'updateLeave', 'submitLeave', 'cancelLeave']) {
    assert.ok(PAGE.includes(fn), `页面没接 ${fn}`);
  }
});

test('列定义覆盖验收要求的字段（单号 / 员工 / 假别 / 区间 / 天数 / 状态 / 操作）', () => {
  const keys = LEAVE_COLUMNS.map((column) => column.colKey);
  for (const key of ['leave_no', 'employee', 'leave_type', 'range', 'duration_days', 'status', 'op']) {
    assert.ok(keys.includes(key), `列表缺列 ${key}`);
  }
});

test('默认 tab：有 list 权限看全员，没有就看自己', () => {
  assert.equal(defaultLeaveTab(true), 'all');
  assert.equal(defaultLeaveTab(false), 'mine');
});

test('路由已注册（漏了 = 点菜单进 404）', () => {
  const router = readFileSync(new URL('../../src/router/index.ts', import.meta.url), 'utf8');
  assert.ok(router.includes("path: 'business/hr/leave'"), '请假单路由没注册');
  assert.ok(router.includes('business/hr/leave/index.vue'), '路由组件路径不对');
  assert.ok(router.includes("permission: 'business:hr:leave:list'"), '路由权限码不对');
});
