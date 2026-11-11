/**
 * 结账检查清单的纯逻辑单测（PERIOD-CLOSE）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * 为什么测 `.ts` 不测 `.vue`：「该谁处理怎么显示」「没有处理人能不能催」「等了多少天」
 * 「催办回执说什么」全在 `period-logic.ts`；`CloseCheckPanel.vue` 只负责把结果画出来。
 * 这些口径一旦漂移（比如有人把「待分配」改成编个名字），`vue-tsc` 抓不到。
 *
 * 断言口径与后端逐字对齐：
 * - `period_items::BlockingItem` 的字段（`doc_type / doc_no / owner_role / owner_name / action_label / route / waiting_since`）
 * - `period_items::doc_type_label` 的中文
 * - `period_remind::RemindRes` 的计数语义（sent / failed / skipped_no_owner / skipped_recent / total / message）
 * - 期间催办权限码：`V0207__erp_period_remind.up.sql`
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  PERIOD_ACTION_PERMISSIONS,
  blockingDocTypeLabel,
  blockingItemCount,
  blockingMoneyText,
  blockerHasItems,
  canRemindItem,
  ownerRoleLabel,
  ownerText,
  remindToast,
  remindableItems,
  waitingText,
} from '@/views/business/erp/period/period-logic';

test('催办权限码与后端 V0207 一致', () => {
  assert.equal(PERIOD_ACTION_PERMISSIONS.remind, 'business:erp:period:remind');
});

test('单据类型中文与后端 doc_type_label 一致，未知类型原样返回', () => {
  assert.equal(blockingDocTypeLabel('VOUCHER'), '凭证');
  assert.equal(blockingDocTypeLabel('STOCK_IN'), '收货');
  assert.equal(blockingDocTypeLabel('PURCHASE_ORDER'), '采购');
  assert.equal(blockingDocTypeLabel('STOCK_TRANSFER'), '调拨');
  // 第 5 项 blocker 的单据类型（ARCH-9 §4.11）：不翻译就会在弹窗里显示 COST_PERIOD 原码
  assert.equal(blockingDocTypeLabel('COST_PERIOD'), '成本批次');
  assert.equal(blockingDocTypeLabel('SOMETHING_NEW'), 'SOMETHING_NEW');
  assert.equal(blockingDocTypeLabel(null), '—');
  assert.equal(blockingDocTypeLabel('  '), '—');
});

test('角色码翻译：已知岗位显示中文，未知原样，空 = 待分配', () => {
  assert.equal(ownerRoleLabel('ACCOUNTANT'), '会计');
  assert.equal(ownerRoleLabel('WAREHOUSE_MANAGER'), '仓管');
  assert.equal(ownerRoleLabel('CUSTOM_ROLE_X'), 'CUSTOM_ROLE_X');
  assert.equal(ownerRoleLabel(null), '待分配');
});

test('「该谁处理」文案：角色(姓名) / 只有角色 / 待分配', () => {
  assert.equal(ownerText({ owner_role: 'ACCOUNTANT', owner_name: '李四' }), '会计(李四)');
  assert.equal(ownerText({ owner_role: 'ACCOUNTANT', owner_name: null }), '会计');
  assert.equal(ownerText({ owner_role: null, owner_name: null }), '待分配');
  assert.equal(ownerText({}), '待分配');
});

test('只有「待分配」的行不能催办（后端不猜处理人，前端也不给点）', () => {
  assert.equal(canRemindItem({ owner_role: 'ACCOUNTANT' }), true);
  // 只有人名没有角色码：仍然可催（后端会给具体人），但不显示
  assert.equal(canRemindItem({ owner_name: '李四' }), true);
  assert.equal(canRemindItem({ owner_role: null, owner_name: null }), false);
  assert.equal(canRemindItem({ owner_role: '   ', owner_name: '' }), false);
});

test('可催办明细过滤：待分配的留在表格里但不进催办列表', () => {
  const items = [
    { doc_id: 'a', owner_role: 'ACCOUNTANT', owner_name: '李四' },
    { doc_id: 'b', owner_role: null, owner_name: null },
    { doc_id: 'c', owner_role: 'WAREHOUSE', owner_name: null },
  ];
  assert.deepEqual(
    remindableItems(items).map((item) => item.doc_id),
    ['a', 'c'],
  );
});

test('金额文案：两位小数 + 千分位；空值显示破折号', () => {
  assert.equal(blockingMoneyText('1234'), '¥1,234.00');
  assert.equal(blockingMoneyText('1234.5'), '¥1,234.50');
  assert.equal(blockingMoneyText(0), '¥0.00');
  assert.equal(blockingMoneyText(null), '—');
  assert.equal(blockingMoneyText(undefined), '—');
  assert.equal(blockingMoneyText('not-a-number'), 'not-a-number');
});

test('已等文案：小时 / 天 / 不到 1 小时 / 缺省', () => {
  const now = new Date('2026-11-03T12:00:00Z');
  assert.equal(waitingText('2026-11-03T11:30:00Z', now), '不到 1 小时');
  assert.equal(waitingText('2026-11-03T07:00:00Z', now), '5 小时');
  assert.equal(waitingText('2026-10-31T12:00:00Z', now), '3 天');
  assert.equal(waitingText(null, now), '—');
  assert.equal(waitingText('乱写的时间', now), '—');
  // 未来时间（时钟漂移）不算负数
  assert.equal(waitingText('2026-11-04T12:00:00Z', now), '—');
});

test('明细存在性与计数：空明细退回老样式', () => {
  const blockers = [
    { type: 'DRAFT_VOUCHER', items: [{ doc_id: 'a' }, { doc_id: 'b' }] },
    { type: 'UNBALANCED' },
    { type: 'INVARIANT', items: [] },
  ];
  assert.equal(blockerHasItems(blockers[0]), true);
  assert.equal(blockerHasItems(blockers[1]), false);
  assert.equal(blockerHasItems(blockers[2]), false);
  assert.equal(blockingItemCount(blockers), 2);
  assert.equal(blockingItemCount([]), 0);
});

test('催办回执：优先用后端那句人话，缺失时按计数合成', () => {
  assert.equal(
    remindToast({
      sent: 2,
      failed: 0,
      skipped_no_owner: 1,
      skipped_recent: 0,
      total: 3,
      message: '2026-10：已催办 2 张，1 张未指定处理人',
    }),
    '2026-10：已催办 2 张，1 张未指定处理人',
  );
  assert.equal(
    remindToast({ sent: 0, failed: 0, skipped_no_owner: 0, skipped_recent: 0, total: 0 }),
    '没有可催办的单据',
  );
  assert.equal(
    remindToast({ sent: 1, failed: 1, skipped_no_owner: 0, skipped_recent: 2, total: 4 }),
    '已催办 1 张，2 张 24 小时内已催过，1 张发送失败',
  );
});
