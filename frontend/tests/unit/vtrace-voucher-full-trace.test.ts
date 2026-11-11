/**
 * VTRACE-5：凭证详情「全链路」四段模型（`voucher/voucher-full-trace.ts`）单测。
 *
 * 运行：`pnpm test:unit`。
 *
 * 这一屏最容易犯的不是渲染错，而是**语义错**，所以用例按「语义」写：
 *
 * 1. 四段永远都在，顺序固定（来源单据 → 审批记录 → 本凭证 → 分录明细）；
 * 2. 缺数据时 `empty` 必须**说清是哪一种空**：手工凭证 / 未走审批 / 无权查看 / 加载失败
 *    是四件完全不同的事，糊成一句「无记录」就是把权限问题说成业务事实；
 * 3. 来源单据能跳就给链接（复用 VTRACE-2 的结论），不能跳必须带原因 tooltip；
 * 4. 审批时间线只渲染引擎写的行（节点 · 审批人 · 结论 · 时间 · 意见）；
 * 5. 分录段自带合计与平衡结论，不平衡要露出来。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import type { FlowTraceInstance } from '@/api/business/flow-api';
import type { VoucherEntryLine } from '@/api/business/erp';

import { resolveSourceDocDisplay } from '@/views/business/erp/voucher/voucher-source-link';
import {
  TRACE_APPROVAL_LOAD_FAILED,
  TRACE_MANUAL_EMPTY,
  TRACE_NO_APPROVAL,
  buildFullTrace,
  filledStepCount,
  formatTraceTime,
  instanceStatusLabel,
  taskStatusLabel,
  type FullTraceInput,
} from '@/views/business/erp/voucher/voucher-full-trace';

const STOCK_IN_ID = 'de300000-0000-4000-8000-000000001103';
const VOUCHER_ID = 'de300000-0000-4000-8000-000000003003';

/** 一张采购入库凭证的详情（字段只取全链路用得到的）。 */
function purchaseInboundVoucher() {
  return {
    id: VOUCHER_ID,
    voucher_no: 'PZ-20261101-900003',
    voucher_date: '2026-11-01',
    status: 'POSTED',
    is_red: false,
    posted_at: '2026-11-01T02:30:00Z',
    summary: '采购入库',
    reverse_of_no: null,
    reversed_by_no: null,
    source_type: 'STOCK_IN',
    source_id: STOCK_IN_ID,
    source_no: 'IN-20261101-900003',
    source_doc_no: 'IN-20261101-900003',
    total_debit: '3616.0000',
    total_credit: '3616.0000',
  };
}

/** 一条分录。 */
function entry(over: Partial<VoucherEntryLine>): VoucherEntryLine {
  return {
    id: `e-${over.line_no ?? 1}`,
    line_no: 1,
    summary: '',
    account_id: 'a',
    account_code: '1403',
    account_name: '原材料',
    debit_amount: '0',
    credit_amount: '0',
    aux_supplier_id: null,
    aux_material_id: null,
    aux_warehouse_id: null,
    aux_dept_id: null,
    aux_qty: null,
    ...over,
  } as VoucherEntryLine;
}

/** 一张已通过的采购订单审批（biz_id 指采购单）。 */
function approvedInstance(over: Partial<FlowTraceInstance> = {}): FlowTraceInstance {
  return {
    id: 'inst-1',
    instance_no: 'FLW-20261101-D90006',
    flow_name: '采购审批',
    status: 'APPROVED',
    started_at: '2026-11-01T01:00:00Z',
    ended_at: '2026-11-01T02:00:00Z',
    initiator_name: '张三',
    biz_type: 'PO_APPROVE',
    biz_no: 'PO-20261101-900003',
    visible: true,
    tasks: [
      {
        node_key: 'n_manager',
        node_name: '财务经理审批',
        assignee_name: '李四',
        status: 'APPROVED',
        at: '2026-11-01T01:40:00Z',
        comment: '金额与合同一致',
        is_auto: false,
      },
      {
        node_key: 'n_director',
        node_name: '财务总监审批',
        assignee_name: '',
        status: 'PENDING',
        at: '2026-11-01T01:45:00Z',
        comment: '',
        is_auto: false,
      },
    ],
    ...over,
  };
}

/** 组装一次全链路输入。 */
function makeInput(over: Partial<FullTraceInput> = {}): FullTraceInput {
  const voucher = purchaseInboundVoucher();
  return {
    source: {
      type: voucher.source_type,
      id: voucher.source_id,
      no: voucher.source_no,
      docNo: voucher.source_doc_no,
    },
    sourceDoc: resolveSourceDocDisplay(voucher.source_type, voucher.source_id, voucher.source_no),
    approval: { owner: '', instances: [], error: '' },
    voucher,
    entries: [
      entry({ line_no: 1, account_code: '1403', account_name: '原材料', debit_amount: '3200.0000' }),
      entry({ line_no: 2, account_code: '2221', account_name: '应交税费', debit_amount: '416.0000' }),
      entry({ line_no: 3, account_code: '2202', account_name: '应付账款', credit_amount: '3616.0000' }),
    ],
    ...over,
  };
}

test('四段永远都在，且顺序固定：来源单据 → 审批记录 → 本凭证 → 分录明细', () => {
  const steps = buildFullTrace(makeInput());
  assert.deepEqual(
    steps.map((step) => step.kind),
    ['source', 'approval', 'voucher', 'entries'],
  );
  assert.deepEqual(
    steps.map((step) => step.title),
    ['来源单据', '审批记录', '本凭证', '分录明细'],
  );
});

test('采购入库凭证：来源单据可跳、带类型与单号，金额走凭证借贷合计', () => {
  const [source] = buildFullTrace(makeInput());
  assert.equal(source.lines.length, 1);
  const line = source.lines[0]!;
  assert.equal(line.title, 'IN-20261101-900003', '链接文字用业务单号');
  assert.deepEqual(line.link, { path: '/business/erp/stock-in', query: { in_id: STOCK_IN_ID } });
  assert.ok(line.metas.some((meta) => meta.includes('收货入库单')), '要写出来源类型的中文名');
  assert.ok(line.metas.some((meta) => meta.includes('IN-20261101-900003')));
  assert.ok(line.metas.some((meta) => meta.includes('2026-11-01')), '要写清记账依据日期');
  assert.equal(line.amount, '3,616.00', '金额按凭证口径格式化（不是 3616.0000）');
  assert.ok(line.tip && line.tip.includes('收货入库单'), 'tooltip 要说点过去落到哪');
});

test('来源单据的日期 / 金额：拿到真值就用真值（单据日期），拿不到才写「记账依据日期」', () => {
  // ① 没有真值 → 记账依据口径（原先的行为，不能变）
  const [plain] = buildFullTrace(makeInput());
  assert.ok(plain.lines[0]!.metas.some((meta) => meta.startsWith('记账依据日期：')));
  assert.ok(!plain.lines[0]!.metas.some((meta) => meta.startsWith('单据日期：')));

  // ② 有真值（收货单详情：入库日期 + 价税合计）→ 用真值，且两种口径不并存
  const [real] = buildFullTrace(
    makeInput({ sourceMeta: { date: '2026-10-31', amount: '3616.0000' } }),
  );
  assert.ok(real.lines[0]!.metas.some((meta) => meta === '单据日期：2026-10-31'));
  assert.ok(!real.lines[0]!.metas.some((meta) => meta.startsWith('记账依据日期：')));
  assert.equal(real.lines[0]!.amount, '3,616.00');
});

test('手工凭证：来源单据段是「手工凭证，无来源单据」，不是空白也不是链接', () => {
  const voucher = { ...purchaseInboundVoucher(), source_type: 'MANUAL', source_id: null, source_no: null, source_doc_no: null };
  const [source] = buildFullTrace(
    makeInput({
      voucher,
      source: { type: 'MANUAL', id: null, no: null, docNo: null },
      sourceDoc: resolveSourceDocDisplay('MANUAL', null, null),
    }),
  );
  assert.equal(source.lines.length, 0);
  assert.equal(source.empty, TRACE_MANUAL_EMPTY);
});

test('来源类型未识别：仍然给一行（不当成「没有来源」），并且不给链接', () => {
  const [source] = buildFullTrace(
    makeInput({
      source: { type: 'RECEIVABLE', id: STOCK_IN_ID, no: 'AR-1', docNo: 'AR-1' },
      sourceDoc: resolveSourceDocDisplay('RECEIVABLE', STOCK_IN_ID, 'AR-1'),
    }),
  );
  assert.equal(source.lines.length, 1, '类型不认识不等于没有来源单据');
  assert.equal(source.lines[0]!.link, null);
  assert.ok(source.lines[0]!.tip?.includes('未识别'), 'tooltip 要说清是类型未识别');
});

test('审批段：一张已通过的审批单 + 两行时间线（节点 / 审批人 / 结论 / 时间 / 意见）', () => {
  const [, approval] = buildFullTrace(
    makeInput({ approval: { owner: '上游采购订单 PO-20261101-900003', instances: [approvedInstance()], error: '' } }),
  );
  assert.equal(approval.subtitle, '上游采购订单 PO-20261101-900003', '上游审批必须在标题上写清，不冒充本单审批');
  assert.equal(approval.empty, '');
  assert.equal(approval.lines.length, 3, '1 张审批单 + 2 行待办');
  const [head, first, second] = approval.lines;
  assert.equal(head!.title, 'FLW-20261101-D90006');
  assert.deepEqual(head!.badge, { text: '已通过', theme: 'success' });
  assert.ok(head!.metas.some((meta) => meta.includes('李四') === false && meta.includes('张三')));
  // 第一行待办：结论 + 审批人 + 意见
  assert.equal(first!.badge?.text, '同意');
  assert.equal(first!.note, '金额与合同一致');
  assert.ok(first!.metas.includes('李四'));
  // 第二行待办：未完成 —— 仍然显示，且状态是「待审批」（留痕不能藏）
  assert.equal(second!.badge?.text, '待审批');
  assert.equal(second!.metas[0], '未指派');
});

test('审批段三种「没有内容」严格分开：加载失败 / 没走审批 / 无权查看', () => {
  // ① 加载失败 —— 技术故障
  const [, failed] = buildFullTrace(
    makeInput({ approval: { owner: '', instances: [], error: '网络超时' } }),
  );
  assert.ok(failed.empty.startsWith(TRACE_APPROVAL_LOAD_FAILED));
  assert.ok(failed.empty.includes('网络超时'));

  // ② 查到了 0 张 —— 业务事实
  const [, none] = buildFullTrace(makeInput());
  assert.equal(none.empty, TRACE_NO_APPROVAL);

  // ③ 查到了但无权看明细 —— 单号 / 状态照常，附一句「无权限」
  const [, hidden] = buildFullTrace(
    makeInput({ approval: { owner: '', instances: [approvedInstance({ visible: false, tasks: [] })], error: '' } }),
  );
  assert.equal(hidden.empty, '');
  assert.equal(hidden.lines.length, 1, '无权时不渲染时间线（后端也没下发）');
  assert.equal(hidden.lines[0]!.title, 'FLW-20261101-D90006', '审批存在过这一事实仍然显示');
  assert.ok(hidden.lines[0]!.note?.includes('无权限'));
});

test('本凭证段：号 / 日期 / 状态 / 记账时间 / 摘要 / 借贷合计', () => {
  const [, , voucherStep] = buildFullTrace(makeInput());
  const line = voucherStep.lines[0]!;
  assert.equal(line.title, 'PZ-20261101-900003');
  assert.deepEqual(line.badge, { text: '已记账', theme: 'success' });
  assert.ok(line.metas.some((meta) => meta.includes('2026-11-01')));
  assert.ok(line.metas.some((meta) => meta.startsWith('记账时间')));
  assert.equal(line.note, '采购入库');
  assert.equal(line.amount, '3,616.00');
  assert.ok(voucherStep.footer?.includes('借方合计 3,616.00'));
});

test('红字凭证：带上被红冲的原凭证号', () => {
  const base = purchaseInboundVoucher();
  const [, , voucherStep] = buildFullTrace(
    makeInput({ voucher: { ...base, is_red: true, reverse_of_no: 'PZ-20261101-900003' } }),
  );
  assert.equal(voucherStep.lines.length, 2);
  assert.equal(voucherStep.lines[1]!.title, '红字凭证');
  assert.ok(voucherStep.lines[1]!.metas[0]!.includes('PZ-20261101-900003'));
});

test('分录段：一行一条（借/贷标记 + 科目 + 金额 + 摘要 + 辅助核算），末尾给借贷合计', () => {
  const entries = [
    entry({
      line_no: 1,
      account_code: '1403',
      account_name: '原材料',
      debit_amount: '3200.0000',
      summary: '入库',
      aux_material_id: 'mat-1',
    }),
    entry({ line_no: 2, account_code: '2221', account_name: '应交税费', debit_amount: '416.0000' }),
    entry({ line_no: 3, account_code: '2202', account_name: '应付账款', credit_amount: '3616.0000' }),
  ];
  const [, , , entriesStep] = buildFullTrace(
    makeInput({ entries, auxText: (dim, id) => (dim === 'MATERIAL' && id === 'mat-1' ? '冷轧钢板' : '') }),
  );
  assert.equal(entriesStep.lines.length, 3);
  assert.deepEqual(entriesStep.lines[0]!.badge, { text: '借', theme: 'primary' });
  assert.deepEqual(entriesStep.lines[2]!.badge, { text: '贷', theme: 'warning' });
  assert.equal(entriesStep.lines[0]!.amount, '3,200.00');
  assert.ok(entriesStep.lines[0]!.metas.some((meta) => meta.includes('物料：冷轧钢板')), '辅助核算要翻成人话');
  assert.equal(entriesStep.lines[0]!.note, '入库');
  assert.equal(
    entriesStep.footer,
    '3 行 · 借 3,616.00 / 贷 3,616.00 · 借贷平衡',
  );
});

test('分录不平衡：footer 直接写差额（不假装平）', () => {
  const [, , , entriesStep] = buildFullTrace(
    makeInput({
      entries: [
        entry({ line_no: 1, debit_amount: '100.0000' }),
        entry({ line_no: 2, credit_amount: '90.0000' }),
      ],
    }),
  );
  assert.ok(entriesStep.footer?.includes('不平衡'));
  assert.ok(entriesStep.footer?.includes('10.00'));
});

test('凭证没有任何分录：分录段给「无分录」，不装成正常', () => {
  const [, , , entriesStep] = buildFullTrace(makeInput({ entries: [] }));
  assert.equal(entriesStep.lines.length, 0);
  assert.ok(entriesStep.empty.includes('无分录'));
});

test('辅助核算解析不到名称时退回短 id（绝不显示整串 uuid）', () => {
  const [, , , entriesStep] = buildFullTrace(
    makeInput({
      entries: [
        entry({
          line_no: 1,
          debit_amount: '1.0000',
          aux_supplier_id: 'de300000-0000-4000-8000-000000000999',
        }),
      ],
    }),
  );
  const meta = entriesStep.lines[0]!.metas.find((item) => item.startsWith('供应商：')) ?? '';
  assert.ok(meta.includes('de300000…'), meta);
  assert.ok(!meta.includes('de300000-0000-4000-8000-000000000999'), '不能把整串 uuid 摆到界面上');
});

test('时间按本地时区格式化（不是 UTC 切片）', () => {
  const text = formatTraceTime('2026-11-01T02:30:00Z');
  const date = new Date('2026-11-01T02:30:00Z');
  const pad = (input: number): string => String(input).padStart(2, '0');
  assert.equal(
    text,
    `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(
      date.getMinutes(),
    )}`,
  );
  assert.equal(formatTraceTime(null), '');
  assert.equal(formatTraceTime('not-a-time'), 'not-a-time', '解析不了就原样透出，不吞');
});

test('未知状态原样透出（后端加新取值时不显示空白）', () => {
  assert.equal(instanceStatusLabel('FOO'), 'FOO');
  assert.equal(taskStatusLabel('BAR'), 'BAR');
  assert.equal(instanceStatusLabel('PENDING'), '审批中');
  assert.equal(taskStatusLabel('SKIPPED'), '跳过');
});

test('filledStepCount 只数有内容的段（给计数徽标用）', () => {
  const steps = buildFullTrace(makeInput());
  assert.equal(filledStepCount(steps), 3, '本例：来源 / 本凭证 / 分录有内容，审批段空');
});
