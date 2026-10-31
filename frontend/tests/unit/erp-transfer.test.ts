/**
 * `transfer-logic.ts` 单测（W4c）：状态映射 / 可流转判断 / 建单校验 / 请求体组装 / 审批时间线。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖、无 DOM）。
 *
 * 为什么测 `.ts` 不测 `.vue`：用例跑在没有 DOM 的 node 里，「六种状态长什么样、这一刻能点
 * 哪些按钮、建单该拦哪些错、请求体发什么」全在 `transfer-logic.ts`；`.vue` 只负责渲染。
 * 这些一旦漂移，`vue-tsc` 抓不到，e2e 也只看得见「界面长什么样」。
 *
 * 断言口径照后端迁移抄（`backend/migrations/business/V0140__erp_stock_transfer.up.sql`）：
 * - 状态六值 = `ck_transfer_status`；
 * - 同仓禁止 = `ck_transfer_diff_wh`；
 * - 明细数量 = `ck_transfer_item_qty`（`qty > 0`）；
 * - 状态机时间戳约束 = `ck_biz_transfer_state`。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  OVERDUE_DEFAULT_THRESHOLD_DAYS,
  SERIAL_OPTION_LIMIT,
  TRANSFER_PAGE_SIZE,
  TRANSFER_STATUSES,
  TRANSFER_STATUS_META,
  WITHDRAW_REASON_MAX,
  actionTargetStatus,
  availableActions,
  buildApprovalSteps,
  buildTransferPayload,
  buildTransferQuery,
  canTransition,
  collectDuplicateSerialIds,
  emptyTransferFilter,
  emptyTransferForm,
  emptyTransferItem,
  formatQty,
  formatTime,
  hasTransferFormError,
  isOverdueRow,
  isStatusFilterLocked,
  isTransferFiltered,
  isValidQty,
  itemCountText,
  needsConfirm,
  overdueBadgeText,
  overdueBadgeTooltip,
  overdueSummaryText,
  todayString,
  transferActionHint,
  transferStatusLabel,
  transferStatusTheme,
  transferStatusVariant,
  validateTransferForm,
  withdrawReasonError,
  type TransferFormState,
} from '@/views/business/erp/transfer/transfer-logic';
import type { TransferRow } from '@/api/business/erp';

/** 一个合法的建单表单（各用例只改自己关心的字段） */
function seedForm(overrides: Partial<TransferFormState> = {}): TransferFormState {
  return {
    from_warehouse_id: 'wh-from-0001',
    to_warehouse_id: 'wh-to-0002',
    transfer_date: '2026-10-30',
    reason: '',
    remark: '',
    items: [{ ...emptyTransferItem('row-1'), material_id: 'mat-0001', qty: 2 }],
    ...overrides,
  };
}

// ==================== ① 状态映射：6 种状态 → 中文 + 颜色 ====================

test('① 状态映射：6 种状态 → 中文 + 配色（紫没有语义槽 → primary/dark 顶替）', () => {
  // 六值齐全，没有多余状态（与 ck_transfer_status 逐字一致）
  assert.deepEqual(
    [...TRANSFER_STATUSES],
    ['DRAFT', 'PENDING', 'APPROVED', 'IN_TRANSIT', 'DONE', 'CANCELLED'],
  );
  assert.equal(Object.keys(TRANSFER_STATUS_META).length, 6);

  // 中文标签照卡面 §四·①
  assert.equal(transferStatusLabel('DRAFT'), '草稿');
  assert.equal(transferStatusLabel('PENDING'), '审批中');
  assert.equal(transferStatusLabel('APPROVED'), '已批准');
  assert.equal(transferStatusLabel('IN_TRANSIT'), '在途');
  assert.equal(transferStatusLabel('DONE'), '已完成');
  assert.equal(transferStatusLabel('CANCELLED'), '已取消');

  // 配色：灰 / 橙 / 蓝 / 「紫→primary+dark」/ 绿 / 浅灰
  assert.equal(transferStatusTheme('DRAFT'), 'default');
  assert.equal(transferStatusTheme('PENDING'), 'warning');
  assert.equal(transferStatusTheme('APPROVED'), 'primary');
  assert.equal(transferStatusTheme('IN_TRANSIT'), 'primary');
  assert.equal(transferStatusTheme('DONE'), 'success');
  assert.equal(transferStatusTheme('CANCELLED'), 'default');

  // 「在途」与「已批准」同主题但不同 variant（TDesign 没有紫色槽，见 logic 文件头）
  assert.equal(transferStatusVariant('APPROVED'), 'light');
  assert.equal(transferStatusVariant('IN_TRANSIT'), 'dark');
  assert.notEqual(transferStatusVariant('APPROVED'), transferStatusVariant('IN_TRANSIT'));

  // 未知状态不吞数据：原样回显 + 安全兜底配色
  assert.equal(transferStatusLabel('SOMETHING_NEW'), 'SOMETHING_NEW');
  assert.equal(transferStatusTheme('SOMETHING_NEW'), 'default');
  assert.equal(transferStatusVariant('SOMETHING_NEW'), 'light');
});

// ==================== ② 可流转判断：每种状态显示哪些按钮 ====================

test('② 可流转判断：每种状态的按钮显隐（照卡面 §四·③）', () => {
  const matrix: Record<string, string[]> = {
    DRAFT: ['edit', 'submit', 'remove'],
    PENDING: ['withdraw'],
    APPROVED: ['ship'],
    IN_TRANSIT: ['receive'],
    DONE: [],
    CANCELLED: ['remove'],
  };

  for (const [status, expected] of Object.entries(matrix)) {
    const actions = availableActions(status);
    const on = (Object.keys(actions) as Array<keyof typeof actions>).filter((key) => actions[key]);
    assert.deepEqual(on.sort(), [...expected].sort(), `${status} 的按钮集合不对`);
  }

  // 状态机：与后端 transfer_model::LEGAL_EDGES 逐条一致（P8 起「撤回」= PENDING → CANCELLED）
  assert.equal(canTransition('DRAFT', 'PENDING'), true);
  assert.equal(canTransition('PENDING', 'CANCELLED'), true); // 撤回（取消整单）
  assert.equal(canTransition('DRAFT', 'CANCELLED'), true); // 草稿直接取消
  assert.equal(canTransition('APPROVED', 'IN_TRANSIT'), true);
  assert.equal(canTransition('IN_TRANSIT', 'DONE'), true);
  // 后端 LEGAL_EDGES 里**没有** PENDING → DRAFT：老实现发 DRAFT 会被 409 拒掉（P8 真机实测）
  assert.equal(canTransition('PENDING', 'DRAFT'), false);
  assert.equal(canTransition('DRAFT', 'APPROVED'), false); // 跳过审批
  assert.equal(canTransition('DRAFT', 'DRAFT'), false);
  assert.equal(canTransition('DONE', 'CANCELLED'), false); // 终态
  assert.equal(canTransition('CANCELLED', 'DRAFT'), false);

  // 动作 → 目标状态 / 是否需要二次确认
  assert.equal(actionTargetStatus('submit'), 'PENDING');
  assert.equal(actionTargetStatus('withdraw'), 'CANCELLED');
  assert.equal(actionTargetStatus('ship'), 'IN_TRANSIT');
  assert.equal(actionTargetStatus('receive'), 'DONE');
  assert.equal(actionTargetStatus('edit'), null); // 走 PUT，不是状态流转
  assert.equal(actionTargetStatus('remove'), null); // 走 DELETE
  assert.equal(needsConfirm('ship'), true); // 不可逆
  assert.equal(needsConfirm('receive'), true); // 不可逆
  assert.equal(needsConfirm('withdraw'), true); // 取消是终态 + 要填原因
  assert.equal(needsConfirm('submit'), false);

  // 撤回原因校验（后端 cancel_reason() 的口径：空白视为没给 → 400）
  assert.match(withdrawReasonError(''), /必须填写原因/);
  assert.match(withdrawReasonError('   '), /必须填写原因/);
  assert.equal(withdrawReasonError('需求取消'), '');
  assert.equal(withdrawReasonError('需'.repeat(WITHDRAW_REASON_MAX)), '');
  assert.match(withdrawReasonError('需'.repeat(WITHDRAW_REASON_MAX + 1)), /最长 255/);

  // 灰掉的按钮要有理由（tooltip 不能是空串）
  assert.match(transferActionHint('DONE'), /已完成/);
  assert.equal(transferActionHint('NOPE'), '未知状态：只读');
});

// ==================== ③ 建单校验 ====================

test('③ 建单校验：同仓报错 / 数量 ≤ 0 报错 / 序列号个数 ≠ 数量报错', () => {
  // 基线：合法表单没有任何错
  assert.equal(hasTransferFormError(validateTransferForm(seedForm())), false);

  // 调出仓 == 调入仓 → 报错（ck_transfer_diff_wh 的前端提前拦）
  const sameWh = validateTransferForm(seedForm({ to_warehouse_id: 'wh-from-0001' }));
  assert.match(sameWh.to_warehouse_id ?? '', /不能相同/);
  assert.equal(hasTransferFormError(sameWh), true);

  // 缺仓 / 缺日期
  const missing = validateTransferForm(
    seedForm({ from_warehouse_id: '', to_warehouse_id: '', transfer_date: '' }),
  );
  assert.equal(missing.from_warehouse_id, '请选择调出仓');
  assert.equal(missing.to_warehouse_id, '请选择调入仓');
  assert.equal(missing.transfer_date, '请选择调拨日期');

  // 数量 ≤ 0 → 报错（ck_transfer_item_qty 是 qty > 0，0 也不许）
  for (const badQty of [0, -1, '']) {
    const form = seedForm({
      items: [{ ...emptyTransferItem('row-1'), material_id: 'mat-0001', qty: badQty }],
    });
    assert.match(validateTransferForm(form).rows?.['row-1']?.qty ?? '', /大于 0/, `qty=${String(badQty)}`);
  }
  assert.equal(isValidQty(null), false);
  assert.equal(isValidQty(0.0001), true); // NUMERIC(18,4) 的最小正数
  assert.equal(isValidQty(1.00001), false); // 超过 4 位小数
  assert.equal(isValidQty(Number.NaN), false);

  // 一物一码：序列号个数必须 == 数量
  const serialRow = {
    ...emptyTransferItem('row-1'),
    material_id: 'mat-serial',
    qty: 3,
    serial_required: true,
    serial_ids: ['s1', 's2'],
  };
  assert.match(
    validateTransferForm(seedForm({ items: [serialRow] })).rows?.['row-1']?.serial_ids ?? '',
    /必须选 3 个序列号，当前 2 个/,
  );

  // 对齐了就通过
  const okSerial = { ...serialRow, serial_ids: ['s1', 's2', 's3'] };
  assert.equal(hasTransferFormError(validateTransferForm(seedForm({ items: [okSerial] }))), false);

  // 一物一码的数量必须是整数（半件货没有序列号）
  const halfItem = { ...serialRow, qty: 0.5, serial_ids: ['s1'] };
  assert.match(
    validateTransferForm(seedForm({ items: [halfItem] })).rows?.['row-1']?.qty ?? '',
    /必须是整数/,
  );

  // 跨明细重复序列号（(item_id, serial_id) 唯一键拦不住，V0140 文件头 ③ 要求自己拦）
  const duplicateForm = seedForm({
    items: [
      { ...serialRow, key: 'row-1', qty: 1, serial_ids: ['s-dup'] },
      { ...serialRow, key: 'row-2', qty: 1, serial_ids: ['s-dup'] },
    ],
  });
  const dupErrors = validateTransferForm(duplicateForm);
  assert.deepEqual(collectDuplicateSerialIds(duplicateForm.items), ['s-dup']);
  assert.match(dupErrors.items ?? '', /重复 1 个/);
  assert.match(dupErrors.rows?.['row-2']?.serial_ids ?? '', /已在别的明细里选过/);

  // 至少要有一条明细
  assert.match(validateTransferForm(seedForm({ items: [] })).items ?? '', /至少添加一条明细/);
});

// ==================== ④ 查询参数 / 请求体 ====================

test('④ 查询参数空串不发 / 请求体省略空值 / 明细序列号只在非空时发', () => {
  // 默认分页头总是带上
  const base = buildTransferQuery(emptyTransferFilter(), 1);
  assert.deepEqual(base, { page: 1, page_size: TRANSFER_PAGE_SIZE });

  // 空串一律整键缺席（后端把 ?transfer_no= 当「等于空串」过滤会查出 0 行）
  const filteredQuery = buildTransferQuery(
    { status: 'DRAFT', from_warehouse_id: 'wh-a', to_warehouse_id: '', transfer_no: '  TR-2026  ', in_transit_overdue: false },
    2,
  );
  assert.deepEqual(filteredQuery, {
    page: 2,
    page_size: TRANSFER_PAGE_SIZE,
    status: 'DRAFT',
    from_warehouse_id: 'wh-a',
    transfer_no: 'TR-2026', // 单号 trim 后发
  });
  assert.equal(isTransferFiltered(emptyTransferFilter()), false);
  assert.equal(isTransferFiltered({ ...emptyTransferFilter(), status: 'DONE' }), true);
  assert.equal(isTransferFiltered({ ...emptyTransferFilter(), transfer_no: '   ' }), false);

  // 请求体：trim 后为空的 reason / remark 不发；非一物一码不发 serial_ids
  const payload = buildTransferPayload(
    seedForm({
      reason: '   ',
      remark: ' 分仓补货 ',
      items: [
        { ...emptyTransferItem('row-1'), material_id: 'mat-0001', qty: 5 },
        { ...emptyTransferItem('row-2'), material_id: 'mat-serial', qty: 2, serial_required: true, serial_ids: ['s1', 's2'] },
      ],
    }),
  );
  assert.equal(payload.from_warehouse_id, 'wh-from-0001');
  assert.equal(payload.transfer_date, '2026-10-30');
  assert.equal('reason' in payload, false, '空的 reason 不该出现在请求体里');
  assert.equal(payload.remark, '分仓补货');
  assert.deepEqual(payload.items, [
    { material_id: 'mat-0001', qty: 5 },
    { material_id: 'mat-serial', qty: 2, serial_ids: ['s1', 's2'] },
  ]);

  // 展示口径：NUMERIC 可能是字符串 / 数字 / 带尾零，统一归一
  assert.equal(formatQty('5.0000'), '5');
  assert.equal(formatQty(2.5), '2.5');
  assert.equal(formatQty(null), '—');
  assert.equal(itemCountText({ item_count: 3 } as TransferRow), '3');
  assert.equal(itemCountText({} as TransferRow), '—', '后端没下发 item_count 时不许假装 0');
  assert.equal(formatTime('2026-10-30'), '2026-10-30'); // DATE 列原样
  assert.equal(formatTime(null), '—');

  // 序列号候选上限是后端 page_size 的上限
  assert.equal(SERIAL_OPTION_LIMIT, 200);
});

// ==================== ⑤ 审批时间线 ====================

test('⑤ 审批进度：提交 → 审批人同意 → 完成（时间线三段拼接）', () => {
  const steps = buildApprovalSteps({
    instance: {
      initiator_name: '老六测试号',
      instance_no: 'FLW-202601-000123',
      status: 'APPROVED',
      started_at: '2026-10-30T02:00:00Z',
      ended_at: '2026-10-30T03:00:00Z',
    },
    timeline: [
      {
        assignee_name: '张主管',
        status: 'APPROVED',
        comment: '同意，注意在途仓温度',
        created_at: '2026-10-30T02:00:00Z',
        completed_at: '2026-10-30T03:00:00Z',
        is_auto: false,
        is_self_approval: false,
        current_node_name: '主管审批',
      },
    ],
  });

  assert.equal(steps.length, 3);
  assert.equal(steps[0].what, '提交');
  assert.equal(steps[0].who, '老六测试号');
  assert.equal(steps[1].tone, 'approve');
  assert.equal(steps[1].who, '张主管');
  assert.equal(steps[1].what, '主管审批：同意');
  assert.equal(steps[1].comment, '同意，注意在途仓温度');
  assert.equal(steps[2].who, '系统');
  assert.equal(steps[2].what, '审批通过');

  // 实例还在跑（ended_at = null）→ 尾部是「审批中」的 pending 步，不是「完成」
  const running = buildApprovalSteps({
    instance: {
      initiator_name: '老六测试号',
      instance_no: 'FLW-202601-000124',
      status: 'RUNNING',
      started_at: '2026-10-30T02:00:00Z',
      ended_at: null,
    },
    timeline: [],
  });
  assert.equal(running.length, 2);
  assert.equal(running[1].tone, 'pending');
  assert.equal(running[1].what, '审批中');
  assert.equal(running[1].at, '', '还没发生的时间不许编一个出来');

  // 驳回 → 红色圆点 + 中文
  const rejected = buildApprovalSteps({
    instance: {
      initiator_name: '老六测试号',
      instance_no: 'FLW-202601-000125',
      status: 'REJECTED',
      started_at: '2026-10-30T02:00:00Z',
      ended_at: '2026-10-30T02:30:00Z',
    },
    timeline: [
      {
        assignee_name: '张主管',
        status: 'REJECTED',
        comment: '数量不对',
        created_at: '2026-10-30T02:00:00Z',
        completed_at: '2026-10-30T02:30:00Z',
        is_auto: false,
        is_self_approval: true,
        current_node_name: '主管审批',
      },
    ],
  });
  assert.equal(rejected[1].tone, 'reject');
  assert.equal(rejected[1].isSelf, true);
  assert.equal(rejected[2].what, '审批驳回');

  // 空表单的默认日期是今天（YYYY-MM-DD，不是 ISO 时间戳）
  assert.match(emptyTransferForm().transfer_date, /^\d{4}-\d{2}-\d{2}$/);
  assert.equal(todayString(new Date(2026, 9, 30)), '2026-10-30');
});

// ==================== ⑥ 超期在途（LNK-F12） ====================

/**
 * 口径来源：后端卡 `t_ff20409a` 的交付说明 + `docs/在途调拨超期告警-LNK-F12.md`。
 *
 * 三条硬约束（错了就是线上 400，不是「列表少两行」）：
 * 1. 开关打开 → 请求带 `in_transit_overdue=true`；
 * 2. 开关关闭 → 这个键**整键缺席**（`false` 是「排除超期」，不是「关掉开关」）；
 * 3. 任何情况下都不拼出 `in_transit_overdue=true + status≠IN_TRANSIT`
 *    或 `in_transit_overdue=false + status=IN_TRANSIT`（后端直接 400）。
 */
test('⑥ 超期在途：开关 → 请求参数 / 矛盾组合永不产生 / 红标与角标文案', () => {
  // 1) 开关打开：发 in_transit_overdue=true，且把 status 丢掉（哪怕 filter 里还留着）
  const on = buildTransferQuery({ ...emptyTransferFilter(), status: 'DRAFT', in_transit_overdue: true }, 1);
  assert.equal(on.in_transit_overdue, true);
  assert.equal('status' in on, false, '开关打开时不能同时发 status（后端 400，不是空列表）');
  assert.equal(isTransferFiltered({ ...emptyTransferFilter(), in_transit_overdue: true }), true);
  assert.equal(isStatusFilterLocked({ ...emptyTransferFilter(), in_transit_overdue: true }), true);
  assert.equal(isStatusFilterLocked(emptyTransferFilter()), false);

  // 2) 开关关闭：整键缺席，不发明 in_transit_overdue=false
  const off = buildTransferQuery({ ...emptyTransferFilter(), status: 'IN_TRANSIT' }, 1);
  assert.deepEqual(off, { page: 1, page_size: TRANSFER_PAGE_SIZE, status: 'IN_TRANSIT' });
  assert.equal('in_transit_overdue' in off, false, '关掉开关 = 后端「不筛」，不是「排除超期」');

  // 3) 穷举 (状态 × 开关)：矛盾组合一条都不许出现
  for (const status of ['', ...TRANSFER_STATUSES]) {
    for (const flag of [true, false]) {
      const query = buildTransferQuery({ ...emptyTransferFilter(), status, in_transit_overdue: flag }, 1);
      const sent = query.in_transit_overdue;
      assert.notEqual(sent, false, `不允许发 in_transit_overdue=false（status=${status}）`);
      if (sent === true) {
        assert.equal('status' in query, false, `超期筛选不允许叠状态（status=${status}）`);
      }
      if (query.status === 'IN_TRANSIT') {
        assert.equal(sent, undefined, 'status=IN_TRANSIT 时不能带超期参数');
      }
    }
  }

  // 4) 行内红标：status=IN_TRANSIT 且后端判超期才显示
  const overdueRow = {
    status: 'IN_TRANSIT',
    in_transit_overdue: true,
    overdue_days: 30,
    overdue_alerted_at: '2026-10-31T06:11:10+00:00',
    overdue_alert_count: 1,
  } as TransferRow;
  assert.equal(isOverdueRow(overdueRow), true);
  assert.equal(overdueBadgeText(overdueRow), '超期 30 天');
  assert.match(overdueBadgeTooltip(overdueRow, 7), /已于 .+ 提醒过 1 次/);
  assert.match(overdueBadgeTooltip(overdueRow, 7), /阈值 7 天/);

  assert.equal(overdueBadgeText({ ...overdueRow, status: 'DONE' } as TransferRow), '', '已签收不挂红标');
  assert.equal(overdueBadgeText({ ...overdueRow, in_transit_overdue: false } as TransferRow), '', '后端没判超期就不挂');
  assert.equal(
    overdueBadgeText({ status: 'IN_TRANSIT', in_transit_overdue: true } as TransferRow),
    '超期',
    '缺 overdue_days 时不能显示「超期 undefined 天」',
  );
  assert.match(
    overdueBadgeTooltip({ status: 'IN_TRANSIT', in_transit_overdue: true, overdue_days: 8 } as TransferRow),
    /还没提醒过/,
    '没有警台账时 tooltip 说清「还没提醒」，别给一颗空 tooltip',
  );

  // 5) 页头角标：文案带阈值；0 张不挂红标；旧版后端不给 threshold_days 也不出 undefined
  assert.equal(overdueSummaryText({ threshold_days: 7, total: 3 }), '发货超 7 天未收货 · 3 张');
  assert.equal(
    overdueSummaryText({ threshold_days: 0, total: 2 }),
    `发货超 ${OVERDUE_DEFAULT_THRESHOLD_DAYS} 天未收货 · 2 张`,
  );
  assert.equal(overdueSummaryText({ threshold_days: 7, total: 0 }), '');
  assert.equal(overdueSummaryText(null), '');
});
