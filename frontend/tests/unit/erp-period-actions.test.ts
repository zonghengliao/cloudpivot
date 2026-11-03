/**
 * 会计期间开账 / 结账 / 反结账的纯逻辑单测（ERP-P1）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * 为什么测 `.ts` 不测 `.vue`：「哪个状态出哪个按钮」「没权限是不是**不渲染**」
 * 「反结账原因够不够 10 字」全在 `period-logic.ts`；`.vue` 只负责把结果画出来。
 * 这些口径一旦漂移（比如有人把 `v-if` 改成 `:disabled`），`vue-tsc` 抓不到，
 * 只有把判据写成函数才能钉住。
 *
 * 断言口径全部与后端逐字对齐：
 * - 状态字面量：`ck_biz_period_status` = `NOT_OPENED | OPEN | CLOSED`
 * - 权限码：`V0188__erp_period_close.up.sql` / `period.rs::{PERM_OPEN,PERM_CLOSE,PERM_REOPEN}`
 * - 原因下限：`period.rs::MIN_REOPEN_REASON_CHARS = 10`
 * - 批量上限：`period.rs::MAX_BATCH_MONTHS = 120`
 * - 阻塞项类型：`period.rs::BLOCKER_*`
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  MIN_REOPEN_REASON_CHARS,
  PERIOD_ACTION_LABELS,
  PERIOD_ACTION_PERMISSIONS,
  batchOpenError,
  canRunPeriodAction,
  closeCheckSummary,
  closeReasonError,
  periodActionOf,
  reopenReasonError,
} from '@/views/business/erp/period/period-logic';

/** 造一个「持有给定权限码」的判定函数（与 `useUserStore().hasPermission` 同语义） */
function permissionsOf(...codes: string[]): (code: string) => boolean {
  return (code: string) => codes.includes(code) || codes.includes('*:*:*');
}

test('权限码与后端 V0188 / V0207 逐字一致（四个码必须分开）', () => {
  assert.equal(PERIOD_ACTION_PERMISSIONS.open, 'business:erp:period:open');
  assert.equal(PERIOD_ACTION_PERMISSIONS.close, 'business:erp:period:close');
  assert.equal(PERIOD_ACTION_PERMISSIONS.reopen, 'business:erp:period:reopen');
  // PERIOD-CLOSE：催办另立一码（能给处理人发催办 ≠ 有资格替他把单过账）
  assert.equal(PERIOD_ACTION_PERMISSIONS.remind, 'business:erp:period:remind');
  // 四个值互不相同：能看列表 ≠ 能封账 ≠ 能撤封账 ≠ 能催办
  const codes = Object.values(PERIOD_ACTION_PERMISSIONS);
  assert.equal(new Set(codes).size, 4);
});

test('状态 → 动作：CLOSED 只能反结账，OPEN 只能结账，NOT_OPENED 只能开账', () => {
  assert.equal(periodActionOf('NOT_OPENED'), 'open');
  assert.equal(periodActionOf('OPEN'), 'close');
  assert.equal(periodActionOf('CLOSED'), 'reopen');
  // 脏值 / 空值一律不给动作（宁可少一个按钮，也不要把结账挂到看不懂的状态上）
  for (const bad of ['', '  ', 'AUDITED', 'DERIVED', null, undefined]) {
    assert.equal(periodActionOf(bad), null, `状态 ${String(bad)} 不该有动作`);
  }
  assert.equal(PERIOD_ACTION_LABELS.close, '结账');
});

test('无权限 → 按钮不出（canRunPeriodAction=false，页面用 v-if 而不是 disabled）', () => {
  // 只有开账权限：OPEN 行（要结账）不该出按钮
  const onlyOpen = permissionsOf(PERIOD_ACTION_PERMISSIONS.open);
  assert.equal(canRunPeriodAction('NOT_OPENED', onlyOpen), true);
  assert.equal(canRunPeriodAction('OPEN', onlyOpen), false, '没有 close 码就不能露结账按钮');
  assert.equal(canRunPeriodAction('CLOSED', onlyOpen), false, '没有 reopen 码就不能露反结账按钮');

  // 只有结账权限：开账 / 反结账都不露
  const onlyClose = permissionsOf(PERIOD_ACTION_PERMISSIONS.close);
  assert.equal(canRunPeriodAction('OPEN', onlyClose), true);
  assert.equal(canRunPeriodAction('NOT_OPENED', onlyClose), false);
  assert.equal(canRunPeriodAction('CLOSED', onlyClose), false);

  // 什么权限都没有 → 三个动作全不出
  const none = permissionsOf();
  for (const status of ['NOT_OPENED', 'OPEN', 'CLOSED']) {
    assert.equal(canRunPeriodAction(status, none), false, `${status} 无权限时不该有按钮`);
  }

  // 超管（`*:*:*`）三个动作都出
  const superAdmin = permissionsOf('*:*:*');
  for (const status of ['NOT_OPENED', 'OPEN', 'CLOSED']) {
    assert.equal(canRunPeriodAction(status, superAdmin), true);
  }
});

test('反结账原因：必填且 ≥ 10 字（与后端同一文案口径）', () => {
  assert.equal(MIN_REOPEN_REASON_CHARS, 10);
  assert.match(String(reopenReasonError('')), /必须填写原因/);
  assert.match(String(reopenReasonError('   ')), /必须填写原因/);
  assert.match(String(reopenReasonError(null)), /必须填写原因/);
  // 9 个字 → 报「至少 10 个字（当前 9 字）」
  const nine = reopenReasonError('一二三四五六七八九');
  assert.ok(nine);
  assert.match(nine, /至少 10 个字/);
  assert.match(nine, /当前 9 字/);
  // 刚好 10 个字 → 通过（边界，后端也是 `chars < 10` 才拒）
  assert.equal(reopenReasonError('一二三四五六七八九十'), null);
  // 前后空格不计数
  assert.equal(reopenReasonError('  货物退回需要冲销原凭证  '), null);
  // emoji / 非 BMP 字符按「一个字」算（`[...text].length`，不是 `.length`）
  assert.equal(reopenReasonError('😀😀😀😀😀😀😀😀😀😀'), null);
});

test('结账备注可选，只在超过 200 字时报错', () => {
  assert.equal(closeReasonError(''), null);
  assert.equal(closeReasonError('2026 年 10 月结账'), null);
  assert.match(String(closeReasonError('字'.repeat(201))), /最多 200 字/);
});

test('结账预检摘要：全绿说「可以结账」，有阻塞点名第一条 + 剩余条数', () => {
  assert.equal(
    closeCheckSummary({ can_close: true, blockers: [] }),
    '前置校验全部通过，可以结账',
  );
  const one = closeCheckSummary({
    can_close: false,
    blockers: [{ message: '还有 3 张草稿凭证未过账：结账前必须全部过账或作废' }],
  });
  assert.equal(one, '还有 3 张草稿凭证未过账：结账前必须全部过账或作废');

  const two = closeCheckSummary({
    can_close: false,
    blockers: [{ message: '还有 3 张草稿凭证未过账' }, { message: '借贷不平，差 0.01 元' }],
  });
  assert.match(two, /还有 3 张草稿凭证未过账/);
  assert.match(two, /另有 1 项/);
});

test('批量开账入参：严格 YYYY-MM、from ≤ to、一次最多 120 个月', () => {
  assert.equal(batchOpenError('2026-11', '2027-12'), null, '14 个月合法');
  assert.equal(batchOpenError(' 2026-11 ', ' 2027-12 '), null, '两端空格应被容忍');

  for (const [from, to] of [
    ['2026-1', '2026-12'],
    ['2026-13', '2027-01'],
    ['2026/11', '2026-12'],
    ['', '2026-12'],
    ['2026-11', ''],
  ]) {
    assert.ok(batchOpenError(from, to), `${from} ~ ${to} 必须报错`);
  }

  assert.match(String(batchOpenError('2027-01', '2026-12')), /晚于结束期间/);
  // 120 个月刚好合法，121 个月必须拦
  assert.equal(batchOpenError('2026-01', '2035-12'), null);
  assert.match(String(batchOpenError('2026-01', '2036-01')), /最多开 120 个期间/);
});
