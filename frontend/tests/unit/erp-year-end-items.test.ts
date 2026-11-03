/**
 * 年结检查清单的纯逻辑单测（YEAR-END）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * 为什么测 `.ts` 不测 `.vue`：「哪些阻塞项该上表格」「明细一共几张」「没明细的
 * 阻塞项显示什么」「调拨页认不认 `?transfer_no=`」全在 `year-end-logic.ts` /
 * `transfer-logic.ts` / `shared/blocker-logic.ts`；`BlockerCheckPanel.vue` 只负责画。
 * 这些口径一旦漂移（比如有人给「借贷不平」也造一张空表、或者把 50 条上限改成 150），
 * `vue-tsc` 抓不到。
 *
 * 断言口径与后端逐字对齐：
 * - `year_end.rs::YearEndBlocker`（`type / count / message / items`）
 * - `period_items::ITEM_LIMIT = 50`
 * - `year_end_remind::PERM_REMIND` / `V0208__erp_year_end_remind.up.sql`
 * - `period_items::route_of`（调拨单 `?transfer_no=`）
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  CHECK_ORDER,
  YEAR_END_PERMISSIONS,
  blockersWithItems,
  extraBlockers,
  yearEndCheckSummary,
  yearEndItemCount,
  yearFromQuery,
} from '@/views/business/erp/year-end/year-end-logic';
import { transferNoFromQuery } from '@/views/business/erp/transfer/transfer-logic';
import {
  blockerHasItems,
  blockingDocTypeLabel,
  ownerText,
  canRemindItem,
} from '@/views/business/erp/shared/blocker-logic';
import type { YearEndBlocker, YearEndCheck } from '@/api/business/erp';

/** 造一条阻塞项（默认不带明细）。 */
function blocker(type: string, extra: Partial<YearEndBlocker> = {}): YearEndBlocker {
  return { type, message: `${type} 的说明`, ...extra };
}

/** 造一条明细（只填展示要用的字段）。 */
function item(docNo: string, docId = docNo) {
  return {
    doc_type: 'PURCHASE_ORDER',
    doc_id: docId,
    doc_no: docNo,
    status_label: '已提交待审批',
    action_label: '去审批',
    route: `/business/erp/purchase-order?po_id=${docId}`,
  };
}

/** 造一份年结预检结果。 */
function check(blockers: YearEndBlocker[], canClose = false): YearEndCheck {
  return {
    year: 2026,
    can_close: canClose,
    blockers,
    state: { year: 2026, closed: false } as YearEndCheck['state'],
  };
}

// ==================== ① 权限码（三处必须逐字一致） ====================

test('年结催办权限码与后端 V0208 逐字一致', () => {
  // 后端 `year_end_remind::PERM_REMIND` / 迁移 V0208 / 本常量是同一个串。
  // 拼错任何一处 → 按钮可见但接口 403（这类 bug 只有对串才能挡）。
  assert.equal(YEAR_END_PERMISSIONS.remind, 'business:erp:year-end:remind');
});

test('原有三个年结权限码没被改动（前缀是 yearend，不是 year-end）', () => {
  assert.equal(YEAR_END_PERMISSIONS.read, 'business:erp:yearend:read');
  assert.equal(YEAR_END_PERMISSIONS.close, 'business:erp:yearend:close');
  assert.equal(YEAR_END_PERMISSIONS.reopen, 'business:erp:yearend:reopen');
});

// ==================== ② 哪些阻塞项上表格 ====================

test('只有带明细的阻塞项上表格（其余三类不硬造空表）', () => {
  const list = check([
    blocker('PERIOD_NOT_CLOSED', { count: 2, periods: ['2026-11', '2026-12'] }),
    blocker('DOC_UNFINISHED', { count: 3, items: [item('PO-1'), item('PO-2'), item('PO-3')] }),
    blocker('UNBALANCED', { diff: '10' }),
    blocker('INVARIANT', { count: 1, items: [item('PZ-1')] }),
    blocker('NEXT_YEAR_NOT_OPENED', { count: 12 }),
  ]);
  const withItems = blockersWithItems(list);
  assert.deepEqual(
    withItems.map((entry) => entry.type),
    ['DOC_UNFINISHED', 'INVARIANT'],
    '只有「全年未完成单据」与「不变量坏凭证」有单据级明细',
  );
  // 空数组 / 缺省都算「没有明细」（后端 `#[serde(default)]` 会给空数组）
  assert.equal(blockersWithItems(check([blocker('DOC_UNFINISHED', { items: [] })])).length, 0);
  assert.equal(blockersWithItems(null).length, 0);
});

test('明细总条数按所有阻塞项累加（每类上限 50 由后端保证）', () => {
  const list = check([
    blocker('DOC_UNFINISHED', { count: 60, items: new Array(50).fill(item('PO')) }),
    blocker('INVARIANT', { count: 1, items: [item('PZ-1')] }),
    blocker('UNBALANCED'),
  ]);
  assert.equal(yearEndItemCount(list), 51);
  assert.equal(yearEndItemCount(null), 0);
});

test('五项校验之外的附加闸仍单独归类（已结转 / 下年期初被占用）', () => {
  const list = check([
    blocker('PERIOD_NOT_CLOSED'),
    blocker('NEXT_YEAR_OPENING_EXISTS', { count: 3 }),
    blocker('ALREADY_CLOSED'),
  ]);
  assert.deepEqual(
    extraBlockers(list).map((entry) => entry.type),
    ['NEXT_YEAR_OPENING_EXISTS', 'ALREADY_CLOSED'],
  );
});

// ==================== ③ 一句话摘要 ====================

test('摘要数「几项通过」而不是「几项失败」', () => {
  // 五项里 3 项通过（只剩期间未结 + 未完成单据）
  const partial = check([blocker('PERIOD_NOT_CLOSED'), blocker('DOC_UNFINISHED')]);
  assert.equal(
    yearEndCheckSummary(partial),
    '2026 年度前置校验 3/5 通过，还有 2 项要处理',
  );
  assert.equal(
    yearEndCheckSummary(check([], true)),
    '前置校验全部通过，可以年结',
  );
  assert.equal(yearEndCheckSummary(null), '还没读到年度状态');
});

test('五项校验的固定顺序与后端 BLOCKER_* 常量一致', () => {
  assert.deepEqual([...CHECK_ORDER], [
    'PERIOD_NOT_CLOSED',
    'DOC_UNFINISHED',
    'UNBALANCED',
    'INVARIANT',
    'NEXT_YEAR_NOT_OPENED',
  ]);
});

// ==================== ④ 共用展示逻辑（抽到 shared 后行为不变） ====================

test('shared/blocker-logic 的口径与结账页一致（抽取不许改行为）', () => {
  assert.equal(blockingDocTypeLabel('STOCK_TRANSFER'), '调拨');
  assert.equal(blockingDocTypeLabel('WHAT'), 'WHAT');
  assert.equal(ownerText({ owner_role: 'ACCOUNTANT', owner_name: '李四' }), '会计(李四)');
  assert.equal(ownerText({ owner_role: 'ACCOUNTANT' }), '会计');
  assert.equal(ownerText({}), '待分配');
  assert.equal(canRemindItem({ owner_role: 'ACCOUNTANT' }), true);
  assert.equal(canRemindItem({}), false);
  assert.equal(blockerHasItems({ items: [item('PO-1')] }), true);
  assert.equal(blockerHasItems({ items: [] }), false);
});

// ==================== ⑤ 调拨页深链 ====================

test('调拨页消费 transfer_no 深链（后端 route_of 的口径）', () => {
  assert.equal(
    transferNoFromQuery({ transfer_no: 'TR-202612-000001' }),
    'TR-202612-000001',
  );
  // 用户手敲成多个同名参数 → 取第一个
  assert.equal(transferNoFromQuery({ transfer_no: ['TR-A', 'TR-B'] }), 'TR-A');
  // 前后空格要吃掉（路由里可能带 %20）
  assert.equal(transferNoFromQuery({ transfer_no: '  TR-C  ' }), 'TR-C');
  // 没带 / 非字符串 → 空串（页面不弹抽屉、不报错）
  assert.equal(transferNoFromQuery({}), '');
  assert.equal(transferNoFromQuery({ transfer_no: 42 }), '');
  assert.equal(transferNoFromQuery({ transfer_no: null }), '');
  assert.equal(transferNoFromQuery(null), '');
});

test('年结页 ?year= 深链（2000–2998 之外一律忽略，退回最新年度）', () => {
  assert.equal(yearFromQuery({ year: '2040' }), 2040);
  assert.equal(yearFromQuery({ year: ' 2037 ' }), 2037);
  assert.equal(yearFromQuery({ year: ['2040', '2041'] }), 2040);
  assert.equal(yearFromQuery({ year: '1999' }), null);
  assert.equal(yearFromQuery({ year: '2999' }), null);
  assert.equal(yearFromQuery({ year: '2040.5' }), null);
  assert.equal(yearFromQuery({ year: '今年' }), null);
  assert.equal(yearFromQuery({}), null);
  assert.equal(yearFromQuery(null), null);
});
