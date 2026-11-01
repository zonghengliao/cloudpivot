/**
 * 年度结转页的纯逻辑单测（ERP-R4）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * 为什么测 `.ts` 不测 `.vue`：「按钮能不能点」「❌ 的行怎么渲染」「反年结原因够不够 10 字」
 * 全在 `year-end-logic.ts`；`.vue` 只负责把结果画出来。判据写成函数才能钉住，
 * 否则有人把 `v-if` 改成 `:disabled`、或把金额 `+` 写成字符串拼接，`vue-tsc` 抓不到。
 *
 * 断言口径与后端逐字对齐：
 * - 权限码：`year_end.rs::PERM_*` / `V0190__erp_year_end.up.sql` 的三个 sys_menu 节点；
 * - 状态：`year_end.rs::{STATUS_CLOSED, STATUS_NOT_CLOSED}` = `CLOSED | NOT_CLOSED`；
 * - 阻塞项类型：`year_end.rs::BLOCKER_*`；
 * - 反年结原因最小字数：`year_end.rs::MIN_REOPEN_REASON_CHARS` = 10。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { YearEndCheck, YearEndState } from '@/api/business/erp';
import {
  BLOCKER_LABELS,
  MIN_REOPEN_REASON_CHARS,
  YEAR_END_PERMISSIONS,
  canClose,
  canReopen,
  checkLines,
  closeBlockedReason,
  closeResultText,
  extraBlockers,
  moneyText,
  openingText,
  reasonError,
  statusLabel,
  statusTheme,
  toMoney,
  voucherText,
} from '@/views/business/erp/year-end/year-end-logic';

/** 造一个「持有给定权限码」的判定函数（与 `useUserStore().hasPermission` 同语义） */
function permissionsOf(...codes: string[]): (code: string) => boolean {
  return (code: string) => codes.includes(code) || codes.includes('*:*:*');
}

/** 一个「未结转」的年度状态 */
function stateOf(overrides: Partial<YearEndState> = {}): YearEndState {
  return {
    year: 2026,
    closed: false,
    status: 'NOT_CLOSED',
    closed_at: null,
    closed_by: null,
    reason: null,
    carry_forward_voucher_id: null,
    carry_forward_voucher_no: null,
    carry_forward_total: '0.0000',
    carry_forward_entries: 0,
    net_profit: '0.0000',
    periods_closed: 12,
    opening_period: '2027-01',
    opening_balance_created: false,
    opening_rows: 0,
    next_year_periods_ready: true,
    next_year_open_count: 12,
    ...overrides,
  };
}

/** 一个「五项全过」的检查结果 */
function okCheckOf(overrides: Partial<YearEndCheck> = {}): YearEndCheck {
  return {
    year: 2026,
    can_close: true,
    blockers: [],
    state: stateOf(),
    ...overrides,
  };
}

test('权限码与后端逐字一致（三个分开的码，不是复用期间码）', () => {
  assert.equal(YEAR_END_PERMISSIONS.read, 'business:erp:yearend:read');
  assert.equal(YEAR_END_PERMISSIONS.close, 'business:erp:yearend:close');
  assert.equal(YEAR_END_PERMISSIONS.reopen, 'business:erp:yearend:reopen');
  assert.equal(MIN_REOPEN_REASON_CHARS, 10);
});

test('金额：字符串 / 数字都能算（后端 Decimal 经 JSON 回来是字符串或数字）', () => {
  assert.equal(toMoney('1234.5000'), 1234.5);
  assert.equal(toMoney(1234.5), 1234.5);
  assert.equal(toMoney(null), 0);
  assert.equal(toMoney(''), 0);
  assert.equal(toMoney('abc'), 0);
  assert.equal(moneyText('1234.5'), '1,234.50');
  // 两个字符串金额相加必须先过 toMoney，否则是拼接
  assert.equal(toMoney('1000.0000') + toMoney('400.0000'), 1400);
});

test('状态：未结转 / 已结转 的中文与 theme', () => {
  assert.equal(statusLabel(stateOf()), '2026 年度未结转');
  assert.equal(statusTheme(stateOf()), 'warning');
  assert.equal(statusLabel(stateOf({ closed: true, status: 'CLOSED' })), '2026 年度已结转');
  assert.equal(statusTheme(stateOf({ closed: true, status: 'CLOSED' })), 'success');
  assert.equal(statusLabel(null), '—');
});

test('五项前置校验固定渲染五行，通过的也渲染 ✅', () => {
  const lines = checkLines(okCheckOf());
  assert.equal(lines.length, 5, '五项固定顺序');
  assert.deepEqual(
    lines.map((line) => line.type),
    ['PERIOD_NOT_CLOSED', 'DOC_UNFINISHED', 'UNBALANCED', 'INVARIANT', 'NEXT_YEAR_NOT_OPENED'],
  );
  assert.ok(lines.every((line) => line.ok), '全过时五行都是 ✅');
  assert.equal(lines[0].label, BLOCKER_LABELS.PERIOD_NOT_CLOSED);
});

test('未结账时：❌ 行点名月份（后端给 periods + message）', () => {
  const check = okCheckOf({
    can_close: false,
    blockers: [
      {
        type: 'PERIOD_NOT_CLOSED',
        periods: ['2026-11', '2026-12'],
        count: 2,
        message: '2026 年还有 2 个月没结账：2026-11、2026-12',
        link: '/business/erp/period?year=2026',
      },
    ],
  });
  const lines = checkLines(check);
  const line = lines.find((item) => item.type === 'PERIOD_NOT_CLOSED');
  assert.ok(line);
  assert.equal(line.ok, false);
  assert.match(line.detail, /2026-11/);
  assert.equal(line.link, '/business/erp/period?year=2026');
  assert.equal(canClose(check, permissionsOf(YEAR_END_PERMISSIONS.close)), false);
});

test('试算不平：detail 里必须带差额', () => {
  const check = okCheckOf({
    can_close: false,
    blockers: [
      { type: 'UNBALANCED', diff: '20', message: '2026 年借贷不平，差 20 元（借方合计 …）' },
    ],
  });
  const line = checkLines(check).find((item) => item.type === 'UNBALANCED');
  assert.ok(line && !line.ok);
  assert.match(line.detail, /差 20/);
});

test('年结按钮：can_close + 有 close 权限 + 未结转 三条同时满足', () => {
  const full = permissionsOf(YEAR_END_PERMISSIONS.close);
  assert.equal(canClose(okCheckOf(), full), true);
  assert.equal(canClose(okCheckOf(), permissionsOf(YEAR_END_PERMISSIONS.read)), false, '没 close 权限');
  assert.equal(canClose(okCheckOf({ can_close: false }), full), false, '前置没过');
  assert.equal(
    canClose(okCheckOf({ state: stateOf({ closed: true, status: 'CLOSED' }) }), full),
    false,
    '已结转不能再来一次',
  );
  assert.equal(canClose(null, full), false);
});

test('年结按钮禁用时必须说清原因（不是静默置灰）', () => {
  const full = permissionsOf(YEAR_END_PERMISSIONS.close);
  assert.equal(closeBlockedReason(okCheckOf(), full), null);
  assert.match(closeBlockedReason(okCheckOf({ can_close: false }), full) ?? '', /前置校验未通过/);
  assert.match(closeBlockedReason(null, full) ?? '', /还没读到/);
  assert.match(
    closeBlockedReason(okCheckOf(), permissionsOf(YEAR_END_PERMISSIONS.read)) ?? '',
    /没有年结权限/,
  );
  assert.match(
    closeBlockedReason(okCheckOf({ state: stateOf({ closed: true }) }), full) ?? '',
    /反年结/,
  );
});

test('反年结按钮：已结转 + 有 reopen 权限', () => {
  const closed = okCheckOf({ state: stateOf({ closed: true, status: 'CLOSED' }) });
  assert.equal(canReopen(closed, permissionsOf(YEAR_END_PERMISSIONS.reopen)), true);
  assert.equal(canReopen(closed, permissionsOf(YEAR_END_PERMISSIONS.close)), false);
  assert.equal(canReopen(okCheckOf(), permissionsOf(YEAR_END_PERMISSIONS.reopen)), false);
});

test('反年结原因：必填 + ≥ 10 字（与后端同判据）', () => {
  assert.match(reasonError('') ?? '', /必须填写原因/);
  assert.match(reasonError(null) ?? '', /必须填写原因/);
  assert.match(reasonError('改一下') ?? '', /至少 10 个字/);
  assert.equal(reasonError('成本结转口径有误，需重做'), null);
  // 中文按字符数算，不按字节
  assert.equal(reasonError('一二三四五六七八九十'), null);
});

test('前置之外的附加阻塞项单独列出（下年期初被占用 / 已结转）', () => {
  const check = okCheckOf({
    can_close: false,
    blockers: [
      { type: 'NEXT_YEAR_OPENING_EXISTS', message: '2027-01 的期初已经有 3 行是手工录的' },
      { type: 'ALREADY_CLOSED', message: '2026 年度已结转' },
    ],
  });
  const extras = extraBlockers(check);
  assert.equal(extras.length, 2);
  assert.deepEqual(
    extras.map((item) => item.type),
    ['NEXT_YEAR_OPENING_EXISTS', 'ALREADY_CLOSED'],
  );
  assert.equal(checkLines(check).filter((line) => !line.ok).length, 0, '五项本身还是绿的');
});

test('结转凭证摘要：空年度跳过凭证时不能说成「失败」', () => {
  const closed = stateOf({ closed: true, status: 'CLOSED', opening_balance_created: true, opening_rows: 12 });
  assert.match(voucherText(closed), /跳过结转凭证/);
  assert.doesNotMatch(voucherText(closed), /年结失败|出错/);
  const withVoucher = stateOf({
    closed: true,
    status: 'CLOSED',
    carry_forward_voucher_no: 'PZ-202612-000123',
    carry_forward_entries: 6,
    carry_forward_total: '1000.0000',
  });
  assert.match(voucherText(withVoucher), /PZ-202612-000123/);
  assert.match(voucherText(withVoucher), /6 条分录/);
  assert.equal(voucherText(null), '—');
});

test('下年期初摘要：生成行数 / 未生成 两种口径', () => {
  assert.match(
    openingText(stateOf({ opening_balance_created: true, opening_rows: 7 })),
    /2027-01 期初已生成（7 行/,
  );
  assert.match(openingText(stateOf()), /尚未生成/);
  assert.equal(openingText(null), '—');
});

test('年结成功文案：带凭证号 / 空年度 两种情况', () => {
  assert.match(
    closeResultText({
      year: 2026,
      carry_forward_voucher_no: 'PZ-202612-000001',
      opening_rows: 9,
    }),
    /PZ-202612-000001.*2027-01 期初已生成 9 行/,
  );
  assert.match(
    closeResultText({ year: 2026, carry_forward_voucher_no: null, opening_rows: 4 }),
    /跳过结转凭证/,
  );
});
