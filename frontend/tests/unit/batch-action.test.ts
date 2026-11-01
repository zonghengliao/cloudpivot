/**
 * 批量操作纯逻辑单测（D7）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * 为什么测 `utils/batch-action.ts` 而不测 `.vue`：卡面要求的六条行为
 * （工具栏出现 + 条数 / 不允许的状态按钮禁用 + tooltip / 进度 / 结果弹窗 / 全选反选清空 /
 * 高危确认 + 填原因）**全部是纯判定**，写在 `.ts` 里才能被 `node:test` 钉住；
 * `BatchActionBar.vue` 只负责把结论画出来。
 *
 * 断言口径与后端逐字对齐：
 * - `BATCH_CHUNK = 20`（前端分批，后端上限 200：`shared/batch.rs::MAX_BATCH_IDS`）；
 * - 失败条目字段 `{ id, no, code, error }`（`shared/batch.rs::BatchFailItem`）；
 * - 结果标题「共 N 条：成功 X 条，失败 Y 条」（`BatchResult::summary`）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  BATCH_CHUNK,
  MAX_BATCH_IDS,
  actionDisabledReason,
  blockedRows,
  chunk,
  confirmMessage,
  failureLines,
  failureText,
  isActionApplicable,
  isExportTask,
  isRowSelectable,
  mergeBatchResults,
  ownerError,
  progressPercent,
  progressText,
  reasonError,
  resultHeadline,
  resultHint,
  selectableIds,
  selectionText,
  toggleSelection,
  type BatchActionSpec,
  type BatchExportTask,
  type BatchOutcome,
  type BatchResult,
  type BatchRow,
} from '@/utils/batch-action';

/** 收货单页的三个动作（与 `views/business/erp/stockin/index.vue` 的声明一致）。 */
const POST: BatchActionSpec = {
  key: 'post',
  label: '批量过账',
  theme: 'primary',
  allowedStatus: ['DRAFT'],
  targetStatus: 'POSTED',
};
const CANCEL: BatchActionSpec = {
  key: 'cancel',
  label: '批量作废',
  theme: 'danger',
  allowedStatus: ['DRAFT', 'POSTED'],
  needConfirm: true,
  requireReason: true,
  minReasonChars: 2,
  targetStatus: 'CANCELLED',
};
const EXPORT: BatchActionSpec = { key: 'export', label: '批量导出', theme: 'default', exportKind: true };
const ACTIONS = [POST, CANCEL, EXPORT];

function row(id: string, status?: string): BatchRow {
  return { id, status };
}

function okResult(ok: string[], failed: Array<{ id: string; no?: string; error: string }>): BatchResult {
  return {
    succeeded: ok.map((id) => ({ id, no: `NO-${id}` })),
    failed: failed.map((item) => ({ id: item.id, no: item.no, code: 10003, error: item.error })),
    total: ok.length + failed.length,
    success_count: ok.length,
    fail_count: failed.length,
  };
}

// ---------- ① 勾选后工具栏出现，显示条数 ----------

test('工具栏文案：勾选后显示条数，并明确「本页 / 全部筛选结果」', () => {
  assert.equal(selectionText(0, 'page', 250), '未勾选', '没勾选时不显示条数');
  assert.equal(selectionText(3, 'page', 250), '已选 3 条（本页，共 250 条筛选结果）');
  assert.equal(selectionText(10, 'page', 10), '已选 10 条（本页）', '恰好一页时不用提筛选数');
  assert.equal(selectionText(250, 'filtered', 250), '已选 250 条（全部筛选结果）');
  // 卡面红线 ④：绝不能只写「已选 N 条」让人以为选了全表
  assert.ok(!selectionText(10, 'page', 250).startsWith('已选 10 条（全部'));
});

// ---------- ② 不满足 allowedStatus 的按钮禁用 + 说明 ----------

test('状态不符合时按钮禁用，提示点名「哪几条状态不对」', () => {
  const rows = [row('a', 'DRAFT'), row('b', 'POSTED'), row('c', 'POSTED')];
  // 批量过账只支持 DRAFT：勾了 2 条已过账 → 禁用
  const reason = actionDisabledReason(POST, rows);
  assert.ok(reason, '有不符合的行必须禁用');
  assert.match(reason ?? '', /批量过账/);
  assert.match(reason ?? '', /DRAFT/);
  assert.match(reason ?? '', /2 条状态不符合/);
  assert.equal(blockedRows(POST, rows).length, 2);

  // 批量作废支持 DRAFT / POSTED → 可以点
  assert.equal(actionDisabledReason(CANCEL, rows), undefined);
  // 导出不限状态 → 可以点
  assert.equal(actionDisabledReason(EXPORT, rows), undefined);

  // 没勾选时也禁用（连请求都不发），报文要说「先勾选」
  assert.match(actionDisabledReason(POST, []) ?? '', /先勾选/);
  // 没权限时优先报权限
  assert.match(actionDisabledReason(POST, rows, false) ?? '', /没有/);
});

test('行复选框：任何动作都不适用的行不可勾选，全选也会跳过它', () => {
  const rows = [row('a', 'POSTED'), row('b', 'CANCELLED')];
  assert.equal(isRowSelectable(ACTIONS, rows[0]), true, '已过账仍可作废');
  assert.equal(
    isRowSelectable([POST], rows[0]),
    false,
    '只声明了「批量过账」的动作组里，已过账行不该被勾'
  );
  assert.deepEqual(selectableIds(ACTIONS, rows), ['a', 'b']);
  // 没有状态列的资源（导出）一律可勾
  assert.equal(isActionApplicable(EXPORT, row('x')), true);
});

// ---------- ③ 批量执行显示进度（并且是分批的） ----------

test('进度：N/M 与百分比，且按 BATCH_CHUNK 分批不超过后端上限', () => {
  assert.equal(progressText(0, 10), '0/10');
  assert.equal(progressText(4, 10), '4/10');
  assert.equal(progressText(10, 10), '10/10');
  assert.equal(progressPercent(4, 10), 40);
  assert.equal(progressPercent(0, 0), 0, '没有行时不能除零');
  assert.equal(progressPercent(20, 10), 100, '超发也不会 >100');

  const ids = Array.from({ length: 45 }, (_, index) => `id-${index}`);
  const parts = chunk(ids, BATCH_CHUNK);
  assert.equal(parts.length, 3, '45 条按 20 一批 = 3 批');
  assert.deepEqual(parts.map((part) => part.length), [20, 20, 5]);
  assert.ok(BATCH_CHUNK <= MAX_BATCH_IDS, '前端每批不能超过后端上限');
  assert.deepEqual(chunk([], BATCH_CHUNK), [], '空数组不产生空批（否则会发空 ids 请求 → 400）');
});

// ---------- ④ 结果弹窗：成功/失败逐条 ----------

test('结果汇总：总数/成功/失败，失败逐条「单号：原因」并可整段复制', () => {
  const merged = mergeBatchResults([
    okResult(['a', 'b'], [{ id: 'c', no: 'IN-3', error: '状态已是已过账' }]),
    okResult(['d'], []),
  ]);
  assert.equal(merged.total, 4);
  assert.equal(merged.success_count, 3);
  assert.equal(merged.fail_count, 1);

  const outcome: BatchOutcome = { kind: 'batch', result: merged };
  assert.equal(resultHeadline(outcome), '共 4 条：成功 3 条，失败 1 条');
  assert.match(resultHint(outcome), /逐条独立事务/);
  assert.deepEqual(failureLines(outcome), ['IN-3：状态已是已过账']);
  assert.equal(failureText(outcome), 'IN-3：状态已是已过账');

  // 没有单号时退到 id —— 复制出去的清单必须能定位到具体哪一条
  const noNo: BatchOutcome = {
    kind: 'batch',
    result: okResult([], [{ id: 'ghost-id', error: '收货入库单不存在' }]),
  };
  assert.deepEqual(failureLines(noNo), ['ghost-id：收货入库单不存在']);

  // 全成功时不能冒出「失败 0 条」的吓人清单
  const allOk: BatchOutcome = { kind: 'batch', result: okResult(['a'], []) };
  assert.deepEqual(failureLines(allOk), []);
  assert.match(resultHint(allOk), /全部处理成功/);

  // 导出：结果不是条数，而是任务号 + 去哪个页面下载
  const task: BatchExportTask = {
    id: 't1',
    task_no: 'EXP20261101-ABCD',
    biz_type: 'erp.stock_in',
    biz_name: '收货入库单',
    export_name: '收货入库单_20261101',
    total: 5,
  };
  const exportOutcome: BatchOutcome = { kind: 'export', task };
  assert.equal(resultHeadline(exportOutcome), '已提交导出任务 EXP20261101-ABCD（5 行）');
  assert.match(resultHint(exportOutcome), /导入导出/);
  assert.deepEqual(failureLines(exportOutcome), []);
  assert.equal(isExportTask(task), true);
  assert.equal(isExportTask(okResult(['a'], [])), false);
});

// ---------- ⑤ 全选 / 反选 / 清空 ----------

test('全选 / 反选 / 清空：只动给定 id，不清其它页的勾选', () => {
  assert.deepEqual(toggleSelection([], ['a', 'b'], true), ['a', 'b'], '全选本页');
  assert.deepEqual(toggleSelection(['a', 'b'], ['a', 'b'], false), [], '反选本页清空');
  assert.deepEqual(
    toggleSelection(['x', 'a'], ['a', 'b'], true),
    ['x', 'a', 'b'],
    '第二页全选不能把第一页的勾选清掉'
  );
  assert.deepEqual(
    toggleSelection(['x', 'a'], ['a'], false),
    ['x'],
    '取消一条只掉这一条'
  );
  // 「选中全部筛选结果」后清空 = 全部不选
  assert.deepEqual(toggleSelection(['a', 'b', 'c'], ['a', 'b', 'c'], false), []);
});

// ---------- ⑥ 高危二次确认 + 必须填原因 ----------

test('高危动作：确认正文带影响条数，原因必填且有最小字数', () => {
  assert.equal(confirmMessage(CANCEL, 3), '确认对 3 条执行「批量作废」？该操作不可撤销，原因会写入单据留痕。');
  assert.match(confirmMessage(POST, 5), /5 条/, '普通动作也要说清影响几条');
  assert.equal(confirmMessage(POST, 5), '确认对 5 条执行「批量过账」？');

  assert.match(reasonError(CANCEL, '') ?? '', /必须填写原因/);
  assert.match(reasonError(CANCEL, '   ') ?? '', /必须填写原因/, '全空格也算没填');
  assert.equal(reasonError(CANCEL, '重复录入'), undefined);
  // 不需要原因的动作不拦
  assert.equal(reasonError(POST, ''), undefined);
  // 最小字数可配（驳回意见这类由后端再判 5 字）
  assert.match(reasonError({ ...CANCEL, minReasonChars: 5 }, '太短') ?? '', /至少 5 个字/);

  // 高危动作必须同时声明「要确认」，否则前端会直接开跑
  assert.equal(CANCEL.needConfirm && CANCEL.requireReason, true);
});

// ---------- ⑦ 批量指派：必须选到具体负责人 ----------

test('批量指派：没选负责人不许提交，选了才通过', () => {
  const assign: BatchActionSpec = { key: 'assign', label: '批量指派', requireOwner: true, needConfirm: true };
  assert.match(ownerError(assign, undefined) ?? '', /必须选择新负责人/);
  assert.match(ownerError(assign, '   ') ?? '', /必须选择新负责人/);
  assert.equal(ownerError(assign, 'user-1'), undefined);
  // 不需要负责人的动作不拦
  assert.equal(ownerError(POST, undefined), undefined);
});
