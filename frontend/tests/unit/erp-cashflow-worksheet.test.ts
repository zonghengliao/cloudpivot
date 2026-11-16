/**
 * 现金流底稿（录入 + 预览）前端纯逻辑单测（M1-18 · 上游 V0414~V0417）。
 *
 * 运行：`pnpm test:unit`（`tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * 为什么测 `.ts` 不测 `.vue`：「金额格式」「哪些行算改动」「提交体长什么样」「上期 null 显示什么」
 * 全在 `worksheet-logic.ts`；`.vue` 只负责画。这些口径漂移时 `vue-tsc` 抓不到
 * （把 `amountOrPlaceholder` 换成 `formatAmount`，`null` 就从「—」变成空串；给 `buildSavePayload`
 * 改成整表提交，12 个从没编过的配置项就被写成「已录 0」），只有写成函数才钉得住。
 *
 * 断言口径与后端逐字对齐（`cashflow_worksheet.rs`）：
 * - 金额是十进制字符串，格式化不过 `Number()`；
 * - 明细行不许为负、最多 4 位小数、整数位 ≤ 14；
 * - `null` ≠ 0（上期没开账 / 上期没编）；
 * - 保存只提交改动过的行（`items` 空 = 不改；`opening`/`closing` 缺省 = 不改）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import type { WorksheetSheet } from '@/api/business/erp/cashflow-worksheet';
import type { AccountPeriodRow } from '@/api/business/erp';
import {
  ACTIVITY_LABELS,
  CASHFLOW_WORKSHEET_REPORT_CODE,
  EMPTY_AMOUNT_PLACEHOLDER,
  PERM_STATEMENT_EXPORT,
  PERM_STATEMENT_LIST,
  PERM_WORKSHEET_LIST,
  PERM_WORKSHEET_SAVE,
  amountOrPlaceholder,
  buildCheckPayload,
  buildDraft,
  buildSavePayload,
  canonicalAmount,
  changedItemCodes,
  checkTone,
  diffText,
  exportBlockedReason,
  formatAmount,
  groupWorksheetRows,
  inputAmount,
  isDraftDirty,
  normalizeAmount,
  periodOptions,
  periodStatusText,
  previousAvailable,
  problemLabel,
  problemText,
  sameAmount,
  statementCheckTone,
  type DraftState,
} from '@/views/business/erp/report/cash-flow/worksheet-logic';

/** 一份「闭合」的底稿夹具：期初 500 + 流入 1200 − 流出 700 = 期末 1000 */
function fixture(overrides: Partial<WorksheetSheet> = {}): WorksheetSheet {
  return {
    period: '2026-11',
    period_status: 'OPEN',
    editable: true,
    rows: [
      {
        line_type: 'FLOW',
        item_code: 'CF-OP-IN-01',
        item_name: '销售商品、提供劳务收到的现金',
        activity: 'OPERATING',
        direction: 'INFLOW',
        amount: '1200.0000',
        sort_order: 10,
        remark: null,
        entered: true,
        item_active: true,
      },
      {
        line_type: 'FLOW',
        item_code: 'CF-OP-IN-02',
        item_name: '收到的税费返还',
        activity: 'OPERATING',
        direction: 'INFLOW',
        amount: '0.0000',
        sort_order: 20,
        remark: null,
        entered: false,
        item_active: true,
      },
      {
        line_type: 'FLOW',
        item_code: 'CF-OP-OUT-01',
        item_name: '购买商品、接受劳务支付的现金',
        activity: 'OPERATING',
        direction: 'OUTFLOW',
        amount: '700.0000',
        sort_order: 30,
        remark: '含预付 100',
        entered: true,
        item_active: true,
      },
      {
        line_type: 'FLOW',
        item_code: 'CF-INV-OUT-01',
        item_name: '购建固定资产支付的现金',
        activity: 'INVESTING',
        direction: 'OUTFLOW',
        amount: '0.0000',
        sort_order: 40,
        remark: null,
        entered: false,
        item_active: false,
      },
    ],
    opening: { line_type: 'OPENING', item_name: '期初现金及现金等价物余额', amount: '500.0000', remark: null },
    closing: {
      line_type: 'CLOSING',
      item_name: '期末现金及现金等价物余额',
      amount: '1000.0000',
      remark: '与银行对账单一致',
    },
    activities: [
      {
        code: 'OPERATING',
        name: '一、经营活动产生的现金流量',
        inflow: '1200.0000',
        outflow: '700.0000',
        net: '500.0000',
        entered_rows: 2,
      },
      {
        code: 'INVESTING',
        name: '二、投资活动产生的现金流量',
        inflow: '0.0000',
        outflow: '0.0000',
        net: '0.0000',
        entered_rows: 0,
      },
      {
        code: 'FINANCING',
        name: '三、筹资活动产生的现金流量',
        inflow: '0.0000',
        outflow: '0.0000',
        net: '0.0000',
        entered_rows: 0,
      },
    ],
    totals: { inflow: '1200.0000', outflow: '700.0000', net: '500.0000' },
    check: {
      balanced: true,
      opening: '500.0000',
      inflow: '1200.0000',
      outflow: '700.0000',
      expected_closing: '1000.0000',
      closing: '1000.0000',
      diff: '0.0000',
      message: '勾稽闭合：期初 500 + 流入 1200 − 流出 700 = 期末 1000',
      problems: [],
    },
    ...overrides,
  };
}

test('权限码 / 报表编码与迁移与后端逐字一致', () => {
  // V0415：底稿读 / 写两码；V0417：出表读 / 导出两码（挂「现金流量表」页面 …001018 的按钮行）
  assert.equal(PERM_WORKSHEET_LIST, 'business:erp:cashflow-worksheet:list');
  assert.equal(PERM_WORKSHEET_SAVE, 'business:erp:cashflow-worksheet:save');
  assert.equal(PERM_STATEMENT_LIST, 'business:erp:cashflow-statement:list');
  assert.equal(PERM_STATEMENT_EXPORT, 'business:erp:cashflow-statement:export');
  // cashflow_statement::REPORT_CODE
  assert.equal(CASHFLOW_WORKSHEET_REPORT_CODE, 'CASH_FLOW_WORKSHEET');
});

test('金额格式化：十进制字符串四舍五入到分，不走 Number()', () => {
  assert.equal(formatAmount('1234.5650'), '1,234.57');
  assert.equal(formatAmount('1200.0000'), '1,200.00');
  assert.equal(formatAmount('-2120.0000'), '-2,120.00');
  assert.equal(formatAmount('0.0000'), '0.00');
  // Number('0.595').toFixed(2) === '0.59'，报表上少一分是事故
  assert.equal(formatAmount('0.5950'), '0.60');
  assert.equal(formatAmount('999.9950'), '1,000.00');
  assert.equal(formatAmount(null), '');
});

test('null 是「没有数」不是 0：上期列画占位符', () => {
  assert.equal(amountOrPlaceholder(null), EMPTY_AMOUNT_PLACEHOLDER);
  assert.equal(amountOrPlaceholder(undefined), EMPTY_AMOUNT_PLACEHOLDER);
  assert.equal(amountOrPlaceholder('0.0000'), '0.00');
  assert.equal(previousAvailable({ previous: null }), false);
  assert.equal(previousAvailable({ previous: '0' }), true);
});

test('规范形：尾零 / 前导零 / -0 都归一，输入框不让人数零', () => {
  assert.equal(canonicalAmount('1200.0000'), '1200');
  assert.equal(canonicalAmount('1200.1200'), '1200.12');
  assert.equal(canonicalAmount('0001200.5'), '1200.5');
  assert.equal(canonicalAmount('-0.0000'), '0');
  assert.equal(canonicalAmount('0.0000'), '0');
  assert.equal(canonicalAmount(''), '');
  // 不认识的形状原样返回，不静默改成 0
  assert.equal(canonicalAmount('abc'), 'abc');
  assert.equal(inputAmount('1500.0000'), '1500');
  assert.equal(inputAmount(null), '');
  // 数值相等（不是字符串相等）——否则「没改过」会被判成改动
  assert.equal(sameAmount('1200.0000', '1200'), true);
  assert.equal(sameAmount('1200.0000', '1200.0001'), false);
});

test('金额校验：4 位小数 / 明细行不许为负 / 整数 14 位 / 非十进制数字', () => {
  assert.deepEqual(normalizeAmount('1200', false), { kind: 'ok', value: '1200' });
  assert.deepEqual(normalizeAmount(' 1200.1234 ', false), { kind: 'ok', value: '1200.1234' });
  assert.deepEqual(normalizeAmount('0', false), { kind: 'ok', value: '0' });
  assert.deepEqual(normalizeAmount('', false), { kind: 'empty' });

  const fiveDecimals = normalizeAmount('1200.12345', false);
  assert.equal(fiveDecimals.kind, 'error');
  assert.match((fiveDecimals as { message: string }).message, /4 位小数/);

  const negativeFlow = normalizeAmount('-5', false);
  assert.equal(negativeFlow.kind, 'error');
  assert.match((negativeFlow as { message: string }).message, /不能为负/);
  // 余额行允许负（透支 / 受限资金）
  assert.deepEqual(normalizeAmount('-5', true), { kind: 'ok', value: '-5' });
  // -0 归一成 0，不算负数
  assert.deepEqual(normalizeAmount('-0', false), { kind: 'ok', value: '0' });

  const tooWide = normalizeAmount('100000000000000', false); // 15 位
  assert.equal(tooWide.kind, 'error');
  assert.match((tooWide as { message: string }).message, /14 位/);

  for (const bad of ['1e3', '1,200', '¥1200', '1200.', '1 200', '+', '1.2.3']) {
    assert.equal(normalizeAmount(bad, false).kind, 'error', `「${bad}」必须被拒`);
  }
});

test('草稿：未录行留空（不是 0）、已录行去尾零、备注原样带出', () => {
  const draft = buildDraft(fixture());
  assert.equal(draft.opening, '500');
  assert.equal(draft.closing, '1000');
  assert.equal(draft.closingRemark, '与银行对账单一致');
  assert.equal(draft.rows['CF-OP-IN-01'].amount, '1200');
  assert.equal(draft.rows['CF-OP-OUT-01'].remark, '含预付 100');
  // 未录入的配置项金额必须留空：0 是「本期确实是零」，空是「还没编」，勾稽上是相反结论
  assert.equal(draft.rows['CF-OP-IN-02'].amount, '');
  assert.equal(draft.rows['CF-INV-OUT-01'].amount, '');
});

test('改动识别：只有真改过的行才算（尾零差异不算）', () => {
  const sheet = fixture();
  const draft = buildDraft(sheet);
  assert.equal(isDraftDirty(draft, sheet), false);
  assert.deepEqual(changedItemCodes(draft, sheet), []);

  draft.rows['CF-OP-IN-01'].amount = '1200.0000'; // 同一个数
  assert.equal(isDraftDirty(draft, sheet), false);

  draft.rows['CF-OP-IN-02'].amount = '0'; // 从「未录」改成「本期为零」
  assert.equal(isDraftDirty(draft, sheet), true);
  assert.deepEqual(changedItemCodes(draft, sheet), ['CF-OP-IN-02']);
});

test('提交体：只发改动过的行，且未录行不会被写成「已录 0」', () => {
  const sheet = fixture();
  const draft: DraftState = buildDraft(sheet);

  // ① 什么都没改 → 不发请求
  assert.equal(buildSavePayload('2026-11', draft, sheet), null);

  // ② 只改一条明细 → items 只有它，两个余额不出现在提交体里
  draft.rows['CF-OP-IN-01'].amount = '1300';
  const one = buildSavePayload('2026-11', draft, sheet);
  assert.ok(one && 'payload' in one);
  const payload = (one as { payload: { period: string; opening?: unknown; closing?: unknown; items: unknown[] } }).payload;
  assert.equal(payload.period, '2026-11');
  assert.equal(payload.opening, undefined);
  assert.equal(payload.closing, undefined);
  assert.deepEqual(payload.items, [{ item_code: 'CF-OP-IN-01', amount: '1300', remark: null }]);

  // ③ 只改期初 → items 为空数组（= 明细行不动），opening 带上新值
  const draft2 = buildDraft(sheet);
  draft2.opening = '600';
  const two = buildSavePayload('2026-11', draft2, sheet);
  assert.ok(two && 'payload' in two);
  assert.deepEqual(
    (two as { payload: { opening: unknown; items: unknown[] } }).payload.opening,
    { amount: '600', remark: null },
  );
  assert.deepEqual((two as { payload: { items: unknown[] } }).payload.items, []);

  // ④ 期初允许负
  const draft3 = buildDraft(sheet);
  draft3.opening = '-300.5000';
  const three = buildSavePayload('2026-11', draft3, sheet);
  assert.deepEqual(
    (three as { payload: { opening: unknown } }).payload.opening,
    { amount: '-300.5', remark: null },
  );
});

test('提交体：非法金额在入口被拒（带项目名），不靠后端 400', () => {
  const sheet = fixture();

  const fiveDecimals = buildDraft(sheet);
  fiveDecimals.rows['CF-OP-IN-01'].amount = '1300.00001';
  const bad = buildSavePayload('2026-11', fiveDecimals, sheet);
  assert.ok(bad && 'error' in bad);
  assert.match((bad as { error: string }).error, /销售商品、提供劳务收到的现金/);
  assert.match((bad as { error: string }).error, /4 位小数/);

  // 明细行为负：方向已由 INFLOW/OUTFLOW 表达
  const negative = buildDraft(sheet);
  negative.rows['CF-OP-OUT-01'].amount = '-700';
  const negResult = buildSavePayload('2026-11', negative, sheet);
  assert.match((negResult as { error: string }).error, /不能为负/);

  // 已录行被清空：不许把「清空」当 0 提交
  const cleared = buildDraft(sheet);
  cleared.rows['CF-OP-IN-01'].amount = '';
  const clearedResult = buildSavePayload('2026-11', cleared, sheet);
  assert.match((clearedResult as { error: string }).error, /请填 0/);

  // 期初填了备注却没金额
  const remarkOnly = buildDraft(sheet);
  remarkOnly.opening = '';
  remarkOnly.openingRemark = '上月结转';
  const remarkResult = buildSavePayload('2026-11', remarkOnly, sheet);
  assert.match((remarkResult as { error: string }).error, /期初余额/);
});

test('试算提交体：不落库、默认不要求闭合、只带改动值', () => {
  const sheet = fixture();
  const draft = buildDraft(sheet);
  draft.closing = '900'; // 故意录错 100

  const checked = buildCheckPayload('2026-11', draft, sheet);
  assert.ok('payload' in checked);
  const payload = (checked as { payload: { require_balanced: boolean; closing?: unknown; items: unknown[] } }).payload;
  assert.equal(payload.require_balanced, false);
  assert.deepEqual(payload.closing, { amount: '900', remark: '与银行对账单一致' });
  assert.deepEqual(payload.items, []);

  const strict = buildCheckPayload('2026-11', draft, sheet, { requireBalanced: true });
  assert.equal((strict as { payload: { require_balanced: boolean } }).payload.require_balanced, true);
});

test('分组：按 经营 → 投资 → 筹资、流入在流出之前；停用 / 未录行照样列示', () => {
  const sections = groupWorksheetRows(fixture());
  assert.deepEqual(
    sections.map((section) => section.activity),
    ['OPERATING', 'INVESTING', 'FINANCING'],
  );
  const operating = sections[0];
  assert.deepEqual(
    operating.inflow.map((row) => row.item_code),
    ['CF-OP-IN-01', 'CF-OP-IN-02'],
  );
  assert.deepEqual(
    operating.outflow.map((row) => row.item_code),
    ['CF-OP-OUT-01'],
  );
  assert.equal(operating.name, '一、经营活动产生的现金流量');
  assert.equal(operating.block?.net, '500.0000');
  // 配置已停用但底稿有历史行 / 配置有但本期没录，都要出现在分组里（静默丢就是丢数）
  const investing = sections[1];
  assert.deepEqual(
    investing.outflow.map((row) => row.item_code),
    ['CF-INV-OUT-01'],
  );
  assert.equal(investing.outflow[0].item_active, false);
  // 后端给了 INVESTING 的小计块就用后端的（金额不在前端算）
  assert.equal(investing.block?.outflow, '0.0000');

  // 后端没给某块小计（历史底稿里该活动已无配置项、后端没回这一块）⇒ 本地中文兜底，
  // 不能画一个没有名字、没有小计的空白分区
  const onlyOperating = fixture();
  onlyOperating.activities = onlyOperating.activities.filter((block) => block.code === 'OPERATING');
  const fallback = groupWorksheetRows(onlyOperating);
  const fallbackInvesting = fallback.find((section) => section.activity === 'INVESTING');
  assert.ok(fallbackInvesting);
  assert.equal(fallbackInvesting.block, null);
  assert.equal(fallbackInvesting.name, ACTIVITY_LABELS.INVESTING);
  assert.equal(ACTIVITY_LABELS.FINANCING, '筹资活动');
});

test('勾稽 / 期间 / 差额的展示口径', () => {
  assert.equal(checkTone({ balanced: true }), 'ok');
  assert.equal(checkTone({ balanced: false }), 'bad');
  assert.equal(statementCheckTone({ balanced: false }), 'bad');

  assert.equal(problemLabel('OPENING_MISSING'), '缺期初余额');
  assert.equal(problemLabel('CLOSING_MISSING'), '缺期末余额');
  assert.equal(problemLabel('IMBALANCED'), '勾稽不闭合');
  assert.equal(problemLabel('EMPTY'), '本期未编');
  assert.equal(problemLabel('SOMETHING_NEW'), 'SOMETHING_NEW'); // 新码不能被吞掉

  // 后端 message 已自带短语前缀 → 页面再拼标签会变成「勾稽不闭合：勾稽不闭合：…」，剥一次
  assert.equal(
    problemText({ code: 'IMBALANCED', message: '勾稽不闭合：期初 5000 + 流入 12000 − 流出 7000 = 10000，差 100' }),
    '期初 5000 + 流入 12000 − 流出 7000 = 10000，差 100',
  );
  // 前缀不是该标签 / 未知码：原样返回
  assert.equal(problemText({ code: 'IMBALANCED', message: '别的说法：差 100' }), '别的说法：差 100');
  assert.equal(problemText({ code: 'NEW_CODE', message: 'NEW_CODE：本体' }), '本体');
  assert.equal(problemText({ code: 'EMPTY', message: '' }), '');

  assert.equal(diffText(null), EMPTY_AMOUNT_PLACEHOLDER);
  assert.equal(diffText('0.0000'), '0');
  assert.equal(diffText('0.0001'), '0.00'); // 差 0.0001 也不四舍五入成「闭合」
  assert.equal(diffText('-100'), '-100.00');

  assert.equal(periodStatusText('OPEN'), '未结账');
  assert.equal(periodStatusText('CLOSED'), '已结账');
  assert.equal(periodStatusText('NOT_OPENED'), '未开账');
});

test('导出闸：闭合放行，未闭合给人话（带差额与逐条问题）', () => {
  assert.equal(exportBlockedReason({ balanced: true, message: '勾稽闭合' }, []), null);

  const blocked = exportBlockedReason({ balanced: false, message: '勾稽不闭合：差 100.00' }, [
    { code: 'IMBALANCED', message: '勾稽不闭合：期初 5000 + 流入 12000 − 流出 7000 = 10000，差 100' },
  ]);
  assert.ok(blocked);
  assert.match(blocked as string, /差 100\.00/);
  assert.match(blocked as string, /期初 5000/);
  // 前缀不重复（问题正文不再出现第二个「勾稽不闭合：」）
  assert.equal((blocked as string).match(/勾稽不闭合：/g)?.length, 1);

  const missingClosing = exportBlockedReason({ balanced: false, message: '缺期末余额' }, [
    { code: 'CLOSING_MISSING', message: '未录入「期末现金及现金等价物余额」' },
  ]);
  assert.equal(missingClosing, '缺期末余额（未录入「期末现金及现金等价物余额」）');
});

test('期间下拉：只列已建立的期间、新的在前、已结账照样可选（历史底稿要能看）', () => {
  const rows = [
    { period_code: '2026-11', status: 'OPEN' },
    { period_code: '2026-12', status: 'NOT_OPENED' },
    { period_code: '2026-10', status: 'CLOSED' },
  ] as AccountPeriodRow[];
  assert.deepEqual(periodOptions(rows), [
    { label: '2026-11（未结账）', value: '2026-11' },
    { label: '2026-10（已结账）', value: '2026-10' },
  ]);
});
