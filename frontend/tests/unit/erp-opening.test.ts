/**
 * 期初余额页的纯逻辑单测（ERP-R5）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * 为什么测 `.ts` 不测 `.vue`：「合计怎么算」「不平能不能保存」「结账后是不是只读」
 * 「没权限是不是不渲染」全在 `opening-logic.ts`；`.vue` 只负责把结果画出来。
 * 这些口径一旦漂移（比如有人把 `v-if` 改成 `:disabled`、或把金额 `+` 写成字符串拼接），
 * `vue-tsc` 抓不到，只有把判据写成函数才能钉住。
 *
 * 断言口径与后端逐字对齐：
 * - 权限码：`opening.rs::PERM_*` / `V0189__erp_opening_balance.up.sql` 的三个 sys_menu 节点
 * - 状态：`opening.rs::STATE_*` = `NONE | ENTERED | LOCKED`
 * - 库存期初科目：`opening.rs::{ACCOUNT_INVENTORY, ACCOUNT_INVENTORY_OFFSET}` = `1405` / `3103`
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { OpeningBalance, OpeningEntry } from '@/api/business/erp';
import {
  OPENING_PERMISSIONS,
  balanceOf,
  canSave,
  effectiveEntries,
  inventoryVoucherHint,
  moneyText,
  openingStateLabel,
  openingStateTheme,
  readonlyReason,
  rowError,
  sumEntries,
  toMoney,
  voucherText,
} from '@/views/business/erp/opening/opening-logic';

/** 造一个「持有给定权限码」的判定函数（与 `useUserStore().hasPermission` 同语义） */
function permissionsOf(...codes: string[]): (code: string) => boolean {
  return (code: string) => codes.includes(code) || codes.includes('*:*:*');
}

/** 一条科目期初 */
function entry(code: string, debit: string, credit: string): OpeningEntry {
  return {
    account_id: `id-${code}`,
    account_code: code,
    account_name: code,
    account_category: 'ASSET',
    debit,
    credit,
    remark: null,
  };
}

/** 一份可保存的期初总览（可按需覆盖字段） */
function openingOf(patch: Partial<OpeningBalance> = {}): OpeningBalance {
  const base: OpeningBalance = {
    period: '2026-11',
    period_status: 'OPEN',
    state: 'ENTERED',
    locked: false,
    entries: [entry('1002', '50000.0000', '0'), entry('3001', '0', '50000.0000')],
    debit_total: '50000.0000',
    credit_total: '50000.0000',
    inventory: [],
    inventory_amount: '0',
    inventory_debit_account: '1405',
    inventory_credit_account: '3103',
    combined_debit_total: '50000.0000',
    combined_credit_total: '50000.0000',
    balanced: true,
    difference: '0',
    voucher: {
      id: 'v1',
      voucher_no: 'PZ-202611-000001',
      voucher_date: '2026-11-01',
      status: 'POSTED',
      total_debit: '50000.0000',
      total_credit: '50000.0000',
      entry_count: 2,
    },
    hints: [],
  };
  return { ...base, ...patch };
}

test('三个权限码与 V0189 的 sys_menu 节点逐字一致', () => {
  assert.equal(OPENING_PERMISSIONS.read, 'business:erp:opening:read');
  assert.equal(OPENING_PERMISSIONS.write, 'business:erp:opening:write');
  assert.equal(OPENING_PERMISSIONS.inventory, 'business:erp:opening:inventory');
});

test('金额解析：字符串 / 数字都能量，非法一律 0（不抛错）', () => {
  assert.equal(toMoney('50000.0000'), 50000);
  assert.equal(toMoney(50000), 50000);
  assert.equal(toMoney(null), 0);
  assert.equal(toMoney(''), 0);
  assert.equal(toMoney('abc'), 0);
  assert.equal(toMoney(undefined), 0);
});

test('合计必须是数值相加，不是字符串拼接（Decimal 回来是 "50000.0000"）', () => {
  const totals = sumEntries([entry('1002', '50000.0000', '0'), entry('1405', '1000.0000', '0')]);
  assert.equal(totals.debit, 51000);
  assert.equal(totals.credit, 0);
  assert.equal(moneyText(totals.debit), '51,000.00');
});

test('借贷平衡判定：差额保留两位，浮点噪声不算不平', () => {
  assert.deepEqual(balanceOf(50000, 50000), { balanced: true, difference: 0 });
  assert.deepEqual(balanceOf(50000, 40000), { balanced: false, difference: 10000 });
  assert.equal(balanceOf(0.1 + 0.2, 0.3).balanced, true);
});

test('期初状态 → 中文与配色（NONE 未录 / ENTERED 已录 / LOCKED 已锁定）', () => {
  assert.equal(openingStateLabel('NONE'), '期初未录');
  assert.equal(openingStateLabel('ENTERED'), '期初已录');
  assert.equal(openingStateLabel('LOCKED'), '已锁定');
  assert.equal(openingStateTheme('LOCKED'), 'success');
  assert.equal(openingStateTheme('NONE'), 'warning');
});

test('结账后整页只读 + 顶部文案点名期间；未开账也不能录', () => {
  assert.equal(readonlyReason(openingOf()), null);
  assert.equal(
    readonlyReason(openingOf({ period_status: 'CLOSED', locked: true })),
    '2026-11 已结账，期初不可改',
  );
  assert.match(
    String(readonlyReason(openingOf({ period_status: 'NOT_OPENED' }))),
    /还没开账/,
  );
});

test('保存按钮：无权限 / 不平衡 / 只读 三种情况都不许点', () => {
  const all = permissionsOf('*:*:*');
  assert.equal(canSave(openingOf(), all, 'account'), true);
  assert.equal(canSave(openingOf(), all, 'inventory'), true);

  // 不平等
  assert.equal(canSave(openingOf({ balanced: false }), all, 'account'), false);
  // 已结账
  assert.equal(canSave(openingOf({ locked: true, period_status: 'CLOSED' }), all, 'account'), false);
  // 只有读权限（写按钮根本不渲染，这里再兜一层）
  assert.equal(canSave(openingOf(), permissionsOf(OPENING_PERMISSIONS.read), 'account'), false);
  // 有 write 但没有 inventory → 库存期初不能存
  assert.equal(
    canSave(openingOf(), permissionsOf(OPENING_PERMISSIONS.write), 'inventory'),
    false,
  );
});

test('行级校验：空科目 / 负数 / 借贷同填 都要报出来', () => {
  assert.equal(rowError({ account_code: '', debit: '0', credit: '0' }), '请选择科目');
  assert.equal(rowError({ account_code: '1002', debit: '-1', credit: '0' }), '期初金额不能为负');
  assert.equal(rowError({ account_code: '1002', debit: '1', credit: '1' }), '借方与贷方不能同时填');
  assert.equal(rowError({ account_code: '1002', debit: '100', credit: '0' }), null);
});

test('两列都为 0 的空行不参与提交（后端也会直接丢掉）', () => {
  const rows = [
    { account_code: '1002', debit: '100', credit: '0' },
    { account_code: '3001', debit: '0', credit: '0' },
  ];
  assert.equal(effectiveEntries(rows).length, 1);
});

test('期初凭证摘要：没有凭证时说清楚，有凭证时报号 + 日期 + 分录数', () => {
  assert.equal(voucherText(null), '尚未生成期初凭证');
  assert.equal(voucherText(openingOf({ voucher: null })), '尚未生成期初凭证');
  assert.match(String(voucherText(openingOf())), /PZ-202611-000001/);
  assert.match(String(voucherText(openingOf())), /2 条分录/);
});

test('库存期初提示：金额为 0 时不打扰，非 0 时说明会自动生成 1405 / 3103 两行', () => {
  assert.equal(inventoryVoucherHint(openingOf()), null);
  const hint = inventoryVoucherHint(openingOf({ inventory_amount: '1000.0000' }));
  assert.match(String(hint), /借 1405/);
  assert.match(String(hint), /贷 3103/);
  assert.match(String(hint), /1,000\.00/);
});
