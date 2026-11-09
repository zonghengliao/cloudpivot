/**
 * 合同管理页（A5）单测：14 条，全部跑在无 DOM 的 `tsx --test` 里。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * # 为什么测 `.ts` + 读源码，而不是渲染 `.vue`
 *
 * 本仓没有 happy-dom/jsdom（零新依赖），`tests/unit` 里也没有组件渲染的先例。
 * 但这些用例真正要钉住的是**口径**：
 * - 6 个状态的文案与配色（`constants/status-map.ts`，页面只调它）；
 * - 合法边（`state_machine.rs::CONTRACT_EDGES` —— 直接 `readFileSync` 读 Rust 源码比对，
 *   后端加一条边而前端没跟，这里先红）；
 * - 表单校验（`contract_dto.rs::CreateContractReq::validate` 的同口径）；
 * - 「哪些按钮该出现」（`canEditContract` / `canDeleteContract` / `contractTransitions`）。
 *
 * 渲染层只做「把逻辑结果铺到模板上」，用**源码断言**兜住接线：
 * 编辑入口有没有真的走 `canEditContract`、金额列有没有真的走 `contractAmountText` ——
 * 逻辑对但模板写错，这几条会红。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';

import { statusMeta } from '@/constants/status-map';
import {
  COMMITMENT_NOT_APPLICABLE_TEXT,
  CONTRACT_EDGES,
  CONTRACT_PAGE_SIZE,
  CONTRACT_REASON_EDGES,
  CONTRACT_STATUSES,
  CONTRACT_TYPE_GUIDE,
  buildContractQuery,
  buildCreatePayload,
  buildStatusPayload,
  buildUpdatePayload,
  canDeleteContract,
  canEditContract,
  commitmentViewOf,
  contractAmountText,
  contractEmptyState,
  contractFormFromDetail,
  contractListState,
  contractPartyBKind,
  contractPartyBLabel,
  contractPartyBTip,
  contractPeriodText,
  contractStatusLabel,
  contractStatusTheme,
  contractTransitions,
  contractTypeLabel,
  contractTypeOptions,
  emptyContractFilter,
  emptyContractForm,
  isContractFiltered,
  isContractTerminal,
  isNoOrderContractType,
  needsReason,
  transitionConfirmText,
  openOrderWarning,
  validateContractForm,
  type ContractFormState,
} from '@/views/business/erp/contract/contract-logic';
import type { ContractDetail, ContractRow } from '@/api/business/erp';

// ---------------------------------------------------------------- 夹具

/** 读后端源码（口径的唯一真相） */
function backendSource(relative: string): string {
  return readFileSync(new URL(`../../../backend/src/business/${relative}`, import.meta.url), 'utf8');
}

/** 读前端源码（模板接线断言） */
function frontendSource(relative: string): string {
  return readFileSync(new URL(`../../src/${relative}`, import.meta.url), 'utf8');
}

/** 一行合同（只覆盖断言用到的列） */
function contractRow(overrides: Partial<ContractRow> = {}): ContractRow {
  return {
    id: '018f4b2c-1111-7222-8333-444455556666',
    contract_no: 'HT-20261102-000001',
    name: '2026 年度原材料采购框架合同',
    contract_name: '2026 年度原材料采购框架合同',
    contract_type: 'PURCHASE',
    party_b_type: 'SUPPLIER',
    party_b_id: '018f4b2c-2222-7222-8333-444455556666',
    party_b_name: '北京钢材有限公司',
    party_b_current_name: '北京钢材有限公司',
    amount: '1234567.8900',
    currency_code: 'CNY',
    sign_date: '2026-01-01',
    start_date: '2026-01-02',
    end_date: '2026-12-31',
    status: 'DRAFT',
    status_label: '草稿',
    updated_at: '2026-11-02T10:00:00Z',
    ...overrides,
  };
}

/** 一张合法表单（用例只改要测的那一项） */
function legalForm(overrides: Partial<ContractFormState> = {}): ContractFormState {
  return {
    ...emptyContractForm(),
    name: '2026 年度原材料采购框架合同',
    party_b_id: '018f4b2c-2222-7222-8333-444455556666',
    amount: 100_000,
    sign_date: '2026-01-01',
    start_date: '2026-01-02',
    end_date: '2026-12-31',
    ...overrides,
  };
}

// ---------------------------------------------------------------- ④ 列表渲染

test('④ 列表渲染：有数据时行数与总数正确，空数据时判为空表', () => {
  const rows = [contractRow(), contractRow({ id: 'id-2', contract_no: 'HT-20261102-000002' })];
  const withData = contractListState({ list: rows, total: 42 });

  assert.equal(withData.rows.length, 2, '有 2 行时列表就是 2 行（不多不少）');
  assert.equal(withData.total, 42, '总数取后端 meta，不是当前页行数');
  assert.equal(withData.isEmpty, false);
  assert.equal(withData.totalText, '共 42 条');

  const empty = contractListState({ list: [], total: 0 });
  assert.equal(empty.rows.length, 0);
  assert.equal(empty.isEmpty, true, '空列表必须能被识别出来（页面据此渲染空态）');

  // 后端偶尔会把 list 给成 null（无数据时的老实现），不能因此白屏
  const nullList = contractListState({ list: null, total: 0 });
  assert.equal(nullList.isEmpty, true, 'list=null 也要落到空态');
});

// ---------------------------------------------------------------- ⑤ 列表空态

test('⑤ 列表空态：无数据给友好提示，且区分「筛不到」与「一条都没有」', () => {
  const none = contractEmptyState(false);
  assert.equal(none.title, '还没有合同');
  assert.ok(none.desc.includes('新建合同'), '空库时引导用户去新建');

  const filtered = contractEmptyState(true);
  assert.equal(filtered.title, '没有匹配的合同');
  assert.ok(filtered.desc.includes('重置'), '筛选态下引导用户重置条件');
  assert.notEqual(none.title, filtered.title, '两种空态文案必须不同（下一步动作不一样）');

  // 筛选态判定：任一条件有值即为筛选态
  assert.equal(isContractFiltered(emptyContractFilter()), false);
  assert.equal(isContractFiltered({ ...emptyContractFilter(), status: 'DRAFT' }), true);
  assert.equal(isContractFiltered({ ...emptyContractFilter(), sign_range: ['2026-01-01'] }), true);
  assert.equal(isContractFiltered({ ...emptyContractFilter(), name: '   ' }), false, '纯空格不算筛选');
});

// ---------------------------------------------------------------- ⑥ 状态徽标

test('⑥ 状态徽标：6 个状态的文案与配色（照后端口径 status_label）', () => {
  const expected = [
    ['DRAFT', '草稿', 'default'],
    ['PENDING_APPROVAL', '审批中', 'primary'],
    ['SIGNED', '已签订', 'default'],
    ['ACTIVE', '已生效', 'success'],
    ['COMPLETED', '已完成', 'primary'],
    ['TERMINATED', '已终止', 'warning'],
  ] as const;

  for (const [status, label, theme] of expected) {
    const meta = statusMeta('erp-contract', status);
    assert.equal(meta.label, label, `${status} 的文案`);
    assert.equal(meta.theme, theme, `${status} 的 t-tag 主题（灰/绿/蓝/橙）`);
    assert.ok(meta.dotVar.startsWith('var(--td-'), `${status} 的圆点必须用主题变量，不许裸 hex`);
    // 页面调的就是这两个包装（单一来源，避免页面里再抄一套）
    assert.equal(contractStatusLabel(status), label);
    assert.equal(contractStatusTheme(status), theme);
  }

  // 后端 status_label 与前端映射逐字一致（大小写 / 措辞都不许漂）
  const rust = backendSource('erp/contract_model.rs');
  for (const [status, label] of expected) {
    assert.ok(
      rust.includes(`${status} => "${label}"`),
      `backend contract_model::status_label 里应有一行 \`${status} => "${label}"\``,
    );
  }

  // 卡面写的 CANCELLED 在 DDL / 状态机里都不存在
  assert.ok(!CONTRACT_STATUSES.includes('CANCELLED' as never), '合同没有 CANCELLED 态（草稿作废走软删）');
  const machine = backendSource('shared/state_machine.rs');
  const contractEdgesBlock = machine.slice(
    machine.indexOf('pub const CONTRACT_EDGES'),
    machine.indexOf('pub const CONTRACT_REASON_EDGES'),
  );
  assert.ok(
    !contractEdgesBlock.includes('CANCELLED'),
    '合同的边表里不应出现 CANCELLED（其它单据的 CANCELLED 与此无关）',
  );
  const contractStatusesBlock = backendSource('erp/contract_model.rs').slice(
    backendSource('erp/contract_model.rs').indexOf('pub const STATUSES'),
    backendSource('erp/contract_model.rs').indexOf('pub const STATUS_DRAFT'),
  );
  assert.ok(!contractStatusesBlock.includes('CANCELLED'), '状态白名单里不应出现 CANCELLED');
});

// ---------------------------------------------------------------- ⑦ 状态按钮

test('⑦ 状态按钮：只显示当前状态能走的合法边，且与前端的边表 = 后端状态机', () => {
  // ---- 边表本身：逐条与 Rust 源码比对（不是靠人眼记忆）
  const machine = backendSource('shared/state_machine.rs');
  const edgesBlock = machine.slice(
    machine.indexOf('pub const CONTRACT_EDGES'),
    machine.indexOf('pub const CONTRACT_REASON_EDGES'),
  );
  const rustEdges = [...edgesBlock.matchAll(/\("([A-Z_]+)",\s*"([A-Z_]+)"\)/g)].map((hit) => [hit[1], hit[2]]);
  assert.deepEqual(
    rustEdges,
    CONTRACT_EDGES.map(([from, to]) => [from, to]),
    '前端 CONTRACT_EDGES 必须与 state_machine.rs::CONTRACT_EDGES 逐条一致',
  );

  const reasonBlock = machine.slice(
    machine.indexOf('pub const CONTRACT_REASON_EDGES'),
    machine.indexOf('pub const CONTRACT_LOCKED'),
  );
  const rustReasonEdges = [...reasonBlock.matchAll(/\("([A-Z_]+)",\s*"([A-Z_]+)"\)/g)].map((hit) => [hit[1], hit[2]]);
  assert.deepEqual(
    rustReasonEdges,
    CONTRACT_REASON_EDGES.map(([from, to]) => [from, to]),
    '需要 reason 的边也要与后端一致（两条终止边）',
  );

  // 状态清单本身也不许漂（后端 R-P1-1 加了 PENDING_APPROVAL）
  const contractModel = backendSource('erp/contract_model.rs');
  const statusesBlock = contractModel.slice(
    contractModel.indexOf('pub const STATUSES'),
    contractModel.indexOf('/// 草稿。'),
  );
  const rustStatuses = [...statusesBlock.matchAll(/"([A-Z_]+)"/g)].map((hit) => hit[1]);
  assert.deepEqual(
    rustStatuses,
    [...CONTRACT_STATUSES],
    '前端 CONTRACT_STATUSES 必须与 contract_model.rs::STATUSES 逐条一致（顺序也一致）',
  );

  // 这条边在 R-P1-1 里被删掉了：没人审过的合同不能一步变成已签（后端会 409 + 点名「先提交审批」）
  assert.ok(
    !CONTRACT_EDGES.some(([from, to]) => from === 'DRAFT' && to === 'SIGNED'),
    'DRAFT → SIGNED 已删除，前端不许再给「登记签订」按钮',
  );

  // ---- 按钮矩阵
  assert.deepEqual(
    contractTransitions('DRAFT').map((item) => [item.label, item.target]),
    [['提交审批', 'PENDING_APPROVAL']],
    'DRAFT 只有一条边：提交审批（不是直接登记签订）',
  );
  assert.deepEqual(
    contractTransitions('PENDING_APPROVAL').map((item) => [item.label, item.target]),
    [['审批通过签订', 'SIGNED'], ['驳回/撤回', 'DRAFT']],
    '审批中可以「审批通过签订」或「驳回/撤回」',
  );
  assert.deepEqual(
    contractTransitions('SIGNED').map((item) => [item.label, item.target]),
    [['生效', 'ACTIVE'], ['终止', 'TERMINATED']],
    'SIGNED 可以生效或终止',
  );
  assert.deepEqual(
    contractTransitions('ACTIVE').map((item) => [item.label, item.target]),
    [['完成', 'COMPLETED'], ['终止', 'TERMINATED']],
    'ACTIVE 可以完成或终止',
  );
  assert.deepEqual(contractTransitions('COMPLETED'), [], '终态没有按钮');
  assert.deepEqual(contractTransitions('TERMINATED'), [], '终态没有按钮');
  assert.equal(contractTransitions('COMPLETED').length, 0, '终态没有按钮');
  assert.equal(isContractTerminal('PENDING_APPROVAL'), false, '审批中不是终态（还能通过 / 驳回）');

  // 两条终止边必须带原因；「驳回/撤回」不带（后端 CONTRACT_REASON_EDGES 只有两条终止边，
  // 审批意见是流程待办的必填项，不是状态流转的 reason）
  assert.equal(needsReason('SIGNED', 'TERMINATED'), true);
  assert.equal(needsReason('ACTIVE', 'TERMINATED'), true);
  assert.equal(needsReason('ACTIVE', 'COMPLETED'), false);
  assert.equal(needsReason('DRAFT', 'PENDING_APPROVAL'), false, '提交审批不需要 reason');
  assert.equal(needsReason('PENDING_APPROVAL', 'DRAFT'), false, '撤回不需要 reason（后端只校验两条终止边）');
  assert.equal(needsReason('PENDING_APPROVAL', 'SIGNED'), false);
  assert.equal(
    contractTransitions('ACTIVE').find((item) => item.target === 'TERMINATED')?.needReason,
    true,
  );

  // 后端详情下发的 next_statuses 优先（卡面第 2 点：有就用后端的）
  assert.deepEqual(
    contractTransitions('ACTIVE', ['TERMINATED']).map((item) => item.target),
    ['TERMINATED'],
    '传了 next_statuses 就只按它渲染（不叠加上前端边表）',
  );
  assert.deepEqual(
    contractTransitions('PENDING_APPROVAL', ['SIGNED', 'DRAFT']).map((item) => [item.label, item.target]),
    [['审批通过签订', 'SIGNED'], ['驳回/撤回', 'DRAFT']],
    '详情抽屉用后端 next_statuses 时，文案仍按前端边表给（后端只下发状态字面量）',
  );
  assert.deepEqual(
    contractTransitions('DRAFT', []).map((item) => item.target),
    ['PENDING_APPROVAL'],
    'next_statuses 为空数组时回落到边表（列表行没有这个字段）',
  );

  // 流转请求体：需要 reason 的边才带 reason
  assert.deepEqual(buildStatusPayload('SIGNED', 'TERMINATED', ' 供应商违约 '), {
    target_status: 'TERMINATED',
    reason: '供应商违约',
  });
  assert.deepEqual(buildStatusPayload('ACTIVE', 'COMPLETED', '随手写的'), {
    target_status: 'COMPLETED',
  }, '不需要 reason 的边不带 reason，避免污染状态日志');
});

// ---------------------------------------------------------------- ⑧ 表单：名称

test('⑧ 表单校验：名称为空 → 提交被拦（且不再报其它无关错误）', () => {
  const blank = validateContractForm(legalForm({ name: '   ' }));
  assert.equal(blank.name, '合同名称不能为空');
  assert.equal(blank.party_b_id, undefined, '其它字段合法时不该被牵连报错');

  assert.equal(validateContractForm(legalForm()).name, undefined, '合法名称必须通过');

  const tooLong = validateContractForm(legalForm({ name: 'x'.repeat(201) }));
  assert.ok(tooLong.name?.includes('200'), `超长要点名上限：${tooLong.name}`);

  // 页面在保存前会调它（源码断言：别把校验写在模板里绕过）
  const vue = frontendSource('views/business/erp/contract/index.vue');
  assert.ok(vue.includes('validateContractForm(form)'), '保存前必须先跑 validateContractForm');
});

// ---------------------------------------------------------------- ⑨ 表单：乙方

test('⑨ 表单校验：乙方未选 → 提交被拦', () => {
  const errors = validateContractForm(legalForm({ party_b_id: '' }));
  assert.equal(errors.party_b_id, '请选择乙方供应商');
  assert.equal(errors.name, undefined);
});

// ---------------------------------------------------------------- ⑩ 表单：日期区间

test('⑩ 表单校验：有效期止早于起 → 提交被拦；签订晚于生效 → 也拦', () => {
  const reversed = validateContractForm(legalForm({ start_date: '2026-12-31', end_date: '2026-01-01' }));
  assert.ok(reversed.end_date?.includes('不能早于'), `区间反了要点名：${reversed.end_date}`);

  const signAfterStart = validateContractForm(legalForm({ sign_date: '2026-02-01', start_date: '2026-01-01' }));
  assert.ok(
    signAfterStart.sign_date?.includes('不能晚于'),
    `签订日期晚于生效日必须拦（后端 require_sign_not_after_start）：${signAfterStart.sign_date}`,
  );

  // 同一天是合法的（后端是 `sign > start` 才拒）
  const sameDay = validateContractForm(legalForm({ sign_date: '2026-01-01', start_date: '2026-01-01' }));
  assert.equal(sameDay.sign_date, undefined);
  assert.equal(sameDay.end_date, undefined);

  // 缺日期也拦（卡面要求必填；后端要求流转到 SIGNED 前三件套齐全）
  const missing = validateContractForm(legalForm({ sign_date: '', start_date: '', end_date: '' }));
  assert.equal(missing.sign_date, '请选择签订日期');
  assert.equal(missing.start_date, '请选择有效期起');
  assert.equal(missing.end_date, '请选择有效期止');
});

// ---------------------------------------------------------------- ⑪ 编辑入口

test('⑪ 编辑按钮：非 DRAFT 单不出现编辑入口（删除同理）', () => {
  assert.equal(canEditContract({ status: 'DRAFT' }), true, '草稿可改');
  assert.equal(canEditContract({ status: 'SIGNED' }), false);
  assert.equal(canEditContract({ status: 'ACTIVE' }), false);
  assert.equal(canEditContract({ status: 'COMPLETED' }), false);
  assert.equal(canEditContract({ status: 'TERMINATED' }), false);
  // 后端详情下发的 can_edit 优先（后端改口径，前端立刻跟）
  assert.equal(canEditContract({ status: 'ACTIVE', can_edit: true }), true);
  assert.equal(canEditContract({ status: 'DRAFT', can_edit: false }), false);

  assert.equal(canDeleteContract({ status: 'DRAFT' }), true, '卡面：删除只给 DRAFT');
  assert.equal(canDeleteContract({ status: 'SIGNED' }), false);
  assert.equal(canDeleteContract({ status: 'ACTIVE' }), false);

  // 模板接线：编辑 / 删除入口必须走这两个函数（逻辑对、模板写错也会红）
  const vue = frontendSource('views/business/erp/contract/index.vue');
  assert.ok(vue.includes('v-if="canEditContract(row)"'), '行内编辑入口要用 canEditContract 判');
  assert.ok(vue.includes('v-if="canDeleteContract(row)"'), '行内删除入口要用 canDeleteContract 判');
  assert.ok(
    !/v-if="row\.status === 'DRAFT'"/.test(vue),
    '不要在模板里另写一套状态判断（口径必须收在 contract-logic）',
  );
});

// ---------------------------------------------------------------- ⑫ 金额显示

test('⑫ 金额显示：千分位 + ¥ 前缀', () => {
  assert.equal(contractAmountText('1234567.8900'), '¥1,234,567.89');
  assert.equal(contractAmountText(1000), '¥1,000.00');
  assert.equal(contractAmountText(0.5), '¥0.50');
  assert.equal(contractAmountText(null), '¥0.00', '空值不显示 NaN');

  // 金额列与详情金额都必须走它（否则会出现「一处 ¥、一处裸数字」）
  const vue = frontendSource('views/business/erp/contract/index.vue');
  assert.ok(vue.includes('contractAmountText(row.amount)'), '列表金额列要走 contractAmountText');
  assert.ok(vue.includes('contractAmountText(detail.amount)'), '详情金额也要走它');

  // 有效期文案
  assert.equal(contractPeriodText('2026-01-01', '2026-12-31'), '2026-01-01 ~ 2026-12-31');
  assert.equal(contractPeriodText(null, null), '—');
  assert.equal(contractPeriodText('2026-01-01', null), '2026-01-01 ~ —');
});

// ---------------------------------------------------------------- ⑬ 请求体

test('⑬ 请求体：空筛选不发、区间映射正确；建单/改单不带单号与状态', () => {
  const query = buildContractQuery(emptyContractFilter(), 1);
  assert.deepEqual(query, { page: 1, page_size: CONTRACT_PAGE_SIZE }, '空筛选只发分页参数');

  const full = buildContractQuery(
    {
      ...emptyContractFilter(),
      contract_no: ' HT-2026 ',
      name: '钢材',
      contract_type: 'PURCHASE',
      status: 'ACTIVE',
      party_b_id: '018f4b2c-2222-7222-8333-444455556666',
      sign_range: ['2026-01-01', '2026-01-31'],
      expire_range: ['2026-12-01', '2026-12-31'],
    },
    2,
    50,
  );
  assert.equal(full.contract_no, 'HT-2026', '合同号去首尾空格');
  assert.equal(full.name, '钢材');
  assert.equal(full.status, 'ACTIVE');
  assert.equal(full.party_b_id, '018f4b2c-2222-7222-8333-444455556666');
  assert.equal(full.sign_date_from, '2026-01-01');
  assert.equal(full.sign_date_to, '2026-01-31');
  // 到期日期落后端 `expire_date_from/to`（对应 end_date 列，不是 start_date）
  assert.equal(full.expire_date_from, '2026-12-01');
  assert.equal(full.expire_date_to, '2026-12-31');
  assert.equal(full.page, 2);
  assert.equal(full.page_size, 50);

  // 只选了一个边界时只发那一个（t-date-range-picker 允许半开）
  const halfOpen = buildContractQuery({ ...emptyContractFilter(), sign_range: ['2026-01-01'] }, 1);
  assert.equal(halfOpen.sign_date_from, '2026-01-01');
  assert.equal(halfOpen.sign_date_to, undefined);

  const create = buildCreatePayload(legalForm());
  assert.equal('contract_no' in create, false, '建单请求不许带合同号（后端自己取号）');
  assert.equal('status' in create, false, '建单请求不许带状态（恒 DRAFT）');
  assert.equal(create.amount, 100_000);
  assert.equal(create.sign_date, '2026-01-01');

  const update = buildUpdatePayload(legalForm({ id: 'x', contract_no: 'HT-1', end_date: '', remark: '' }));
  assert.equal('contract_no' in update, false, '改单也不许带合同号（卡面 + 后端都禁止）');
  assert.equal(update.end_date, null, '清空日期要显式传 null（后端 double_option：不传 = 保持原值）');
  assert.equal(update.remark, null);
});

// ---------------------------------------------------------------- ⑭ 表单回填 / 类型下拉

test('⑭ 编辑回填与类型下拉：详情 → 表单字段齐全，类型放开「采购 / 销售合同」', () => {
  const detail: ContractDetail = {
    ...contractRow({
      amount: '226.0000',
      sign_date: null,
      start_date: '2026-03-01',
      end_date: '2027-02-28',
      status: 'DRAFT',
    }),
    party_a_id: null,
    party_a_name: null,
    owner_id: null,
    dept_id: null,
    remark: '月结 30 天',
    created_at: '2026-11-02T10:00:00Z',
    create_by: null,
    update_by: null,
    can_edit: true,
    usable_for_order: false,
    next_statuses: ['SIGNED'],
  };
  const form = contractFormFromDetail(detail);
  assert.equal(form.id, detail.id);
  assert.equal(form.contract_no, 'HT-20261102-000001', '合同号回填到只读展示位');
  assert.equal(form.amount, 226, 'Decimal 字符串要转成 number（t-input-number 只认数字）');
  assert.equal(form.sign_date, '', '后端给 null 时表单给空串（日期组件不认 null）');
  assert.equal(form.start_date, '2026-03-01');
  assert.equal(form.remark, '月结 30 天');

  // 类型下拉：四类全可选（CTR-02-F1 开放 SERVICE / LEASE，不再置灰）
  const options = contractTypeOptions();
  assert.equal(options.length, 4, '后端白名单 4 个值都列出来');
  assert.deepEqual(
    options.map((item) => item.value),
    ['PURCHASE', 'SALES', 'SERVICE', 'LEASE'],
    '四类全开（顺序与 CONTRACT_TYPE_LABELS 一致）',
  );
  assert.deepEqual(
    options.filter((item) => item.disabled).map((item) => item.value),
    [],
    'CTR-02-F1 之后没有任何类型被置灰',
  );
  assert.ok(options.every((item) => !item.label.includes('未开放')), '文案里不再出现「未开放」');
  assert.equal(contractTypeLabel('PURCHASE'), '采购合同');
  assert.equal(contractTypeLabel('SALES'), '销售合同');
  assert.equal(contractTypeLabel('SERVICE'), '服务合同');
  assert.equal(contractTypeLabel('LEASE'), '租赁合同');
  assert.equal(contractTypeLabel(null), '—');
  assert.equal(contractTypeLabel('NOPE'), 'NOPE', '未知类型原样显示，不抛错');
});

// ---------------------------------------------------------------- ⑮ 乙方主档随类型切换（SO-27 / CTR-02-F1）

test('⑮ 乙方主档：SALES → 客户，其余 → 供应商；称谓分四档（Service→服务商 / Lease→出租方）', () => {
  assert.equal(contractPartyBKind('SALES'), 'CUSTOMER');
  assert.equal(contractPartyBKind('sales'), 'CUSTOMER', '大小写不敏感（后端按 to_uppercase 比）');
  assert.equal(contractPartyBKind(' PURCHASE '), 'SUPPLIER');
  assert.equal(contractPartyBKind('SERVICE'), 'SUPPLIER', '服务商在系统里是供应商主档');
  assert.equal(contractPartyBKind('LEASE'), 'SUPPLIER', '出租方在系统里是供应商主档');
  assert.equal(contractPartyBKind(''), 'SUPPLIER');
  assert.equal(contractPartyBKind(undefined), 'SUPPLIER');

  // 称谓四档（label / placeholder / 校验文案同源）
  assert.equal(contractPartyBLabel('PURCHASE'), '乙方供应商');
  assert.equal(contractPartyBLabel('SALES'), '乙方客户');
  assert.equal(contractPartyBLabel('SERVICE'), '乙方服务商');
  assert.equal(contractPartyBLabel('LEASE'), '乙方出租方');
  assert.equal(contractPartyBLabel('lease'), '乙方出租方', '大小写不敏感');
  assert.equal(contractPartyBLabel('NOPE'), '乙方供应商', '未知类型按供应商兜底，不抛错');

  // 来源说明只在服务 / 租赁出现（其余主档一眼就对，不需要解释）
  assert.equal(contractPartyBTip('SERVICE'), '服务商 / 出租方在系统中登记为供应商');
  assert.equal(contractPartyBTip('LEASE'), '服务商 / 出租方在系统中登记为供应商');
  assert.equal(contractPartyBTip('PURCHASE'), '');
  assert.equal(contractPartyBTip('SALES'), '');

  // 后端推导必须同源：`contract_dto.rs` 里 SALES ↔ CUSTOMER 的配对是唯一真相
  const dto = backendSource('erp/contract_dto.rs');
  assert.ok(
    /party_b_type.*CUSTOMER|CUSTOMER.*party_b_type/s.test(dto),
    '后端 contract_dto.rs 里应有 SALES → CUSTOMER 的推导',
  );
  assert.ok(dto.includes('SUPPORTED_TYPES'), '后端 SUPPORTED_TYPES 仍是放行清单的唯一真相');

  // 校验文案随类型分叉（供应商时不再说「请选择乙方客户」）
  const salesErrors = validateContractForm({ ...legalForm(), contract_type: 'SALES', party_b_id: '' });
  assert.equal(salesErrors.party_b_id, '请选择乙方客户');
  const purchaseErrors = validateContractForm({ ...legalForm(), party_b_id: '' });
  assert.equal(purchaseErrors.party_b_id, '请选择乙方供应商');
  const serviceErrors = validateContractForm({ ...legalForm(), contract_type: 'SERVICE', party_b_id: '' });
  assert.equal(serviceErrors.party_b_id, '请选择乙方服务商');
  const leaseErrors = validateContractForm({ ...legalForm(), contract_type: 'LEASE', party_b_id: '' });
  assert.equal(leaseErrors.party_b_id, '请选择乙方出租方');
  // 四类都是合法类型：不该再报「类型无效」
  for (const type of ['PURCHASE', 'SALES', 'SERVICE', 'LEASE']) {
    assert.equal(
      validateContractForm({ ...legalForm(), contract_type: type }).contract_type,
      undefined,
      `${type} 必须能通过提交前校验`,
    );
  }
  // 脏值仍要拦，文案中性（不出现「本批次」）
  const dirty = validateContractForm({ ...legalForm(), contract_type: 'NOPE' });
  assert.ok(dirty.contract_type?.includes('合同类型无效'), `脏值要点名类型无效：${dirty.contract_type}`);
  assert.ok(!dirty.contract_type?.includes('批次'), '文案里不能再出现「批次」');
});

// ---------------------------------------------------------------- ⑯ 履约口径（CTR-02-F1）

test('⑯ 履约口径：ORDER_LINKED 显示真值、NOT_APPLICABLE 只说「不适用」、缺字段降级不渲染', () => {
  // 挂订单的两类：四个字段都是后端真值，前端只搬运不计算
  const linked = commitmentViewOf({
    commitment_scope: 'ORDER_LINKED',
    committed_amount: '226.0000',
    remaining_amount: '9774.0000',
    open_order_count: 2,
    open_order_nos: ['PO-20261109-000001', 'PO-20261109-000002'],
  });
  assert.equal(linked.kind, 'ORDER_LINKED');
  assert.deepEqual(linked, {
    kind: 'ORDER_LINKED',
    committedText: '¥226.00',
    remainingText: '¥9,774.00',
    remainingNegative: false,
    openOrderCount: 2,
    openOrderNos: ['PO-20261109-000001', 'PO-20261109-000002'],
  });

  // 剩余额度允许为负（超签不夹到 0）
  const over = commitmentViewOf({
    commitment_scope: 'order_linked',
    committed_amount: 1200,
    remaining_amount: '-200.00',
    open_order_count: 0,
    open_order_nos: [],
  });
  assert.equal(over.kind, 'ORDER_LINKED');
  assert.equal(over.kind === 'ORDER_LINKED' && over.remainingNegative, true);
  assert.equal(over.kind === 'ORDER_LINKED' && over.remainingText, '¥-200.00');

  // 服务 / 租赁：后端照常下发 committed_amount=0，但口径是不适用 —— 不能当「已执行 0.00」显示
  const notApplicable = commitmentViewOf({
    commitment_scope: 'NOT_APPLICABLE',
    committed_amount: '0.0000',
    remaining_amount: '120000.0000',
    open_order_count: 0,
    open_order_nos: [],
  });
  assert.deepEqual(notApplicable, { kind: 'NOT_APPLICABLE' }, '不带任何金额（模板只渲染灰字）');
  assert.ok(COMMITMENT_NOT_APPLICABLE_TEXT.includes('履约进度不适用'));
  assert.ok(COMMITMENT_NOT_APPLICABLE_TEXT.includes('不走订单勾稽'));

  // 字段缺失 / 未知取值 → 降级为不渲染（不抛错）
  assert.equal(commitmentViewOf(null).kind, 'HIDDEN');
  assert.equal(commitmentViewOf(undefined).kind, 'HIDDEN');
  assert.equal(commitmentViewOf({}).kind, 'HIDDEN');
  assert.equal(
    commitmentViewOf({ commitment_scope: 'PAYMENT_PLAN', committed_amount: '1' }).kind,
    'HIDDEN',
    '预留口径还没做渲染分支 → 先不渲染，而不是显示一个会被误读的 0.00',
  );

  // 类型判定：「不挂订单」只有服务 / 租赁
  assert.equal(isNoOrderContractType('SERVICE'), true);
  assert.equal(isNoOrderContractType('lease'), true);
  assert.equal(isNoOrderContractType('PURCHASE'), false);
  assert.equal(isNoOrderContractType('SALES'), false);
  assert.equal(isNoOrderContractType(null), false);

  // 模板接线：履约区必须走口径分支，且灰字文案来自常量
  const vue = frontendSource('views/business/erp/contract/index.vue');
  assert.ok(vue.includes('data-testid="contract-detail-commitment"'), 'ORDER_LINKED 的履约区');
  assert.ok(vue.includes('data-testid="contract-detail-commitment-na"'), 'NOT_APPLICABLE 的灰字行');
  assert.ok(vue.includes('COMMITMENT_NOT_APPLICABLE_TEXT'), '灰字文案走常量，别在模板里另写一份');
  assert.ok(
    vue.includes('{{ committedText }}') && vue.includes('{{ remainingText }}'),
    '已执行 / 剩余额度只能来自后端字段（模板不自己算、不写死）',
  );
  assert.ok(
    !vue.includes('0.00'),
    '模板里不许出现 0.00 这类硬编码金额（服务 / 租赁不得渲染成「已执行 0」）',
  );
});

// ---------------------------------------------------------------- ⑰ 类型选择引导

test('⑰ 类型选择引导：四行判定表与后端放行清单一致，服务 / 租赁的流转文案不提采购单', () => {
  assert.equal(CONTRACT_TYPE_GUIDE.length, 4, '四行判定表');
  assert.deepEqual(
    CONTRACT_TYPE_GUIDE.map((row) => row.type),
    ['PURCHASE', 'SALES', 'SERVICE', 'LEASE'],
  );
  for (const row of CONTRACT_TYPE_GUIDE) {
    assert.equal(row.label, contractTypeLabel(row.type), `${row.type} 的中文名要与类型映射同源`);
    assert.ok(row.when.trim().length > 0, `${row.type} 的判定说明不能为空`);
  }
  // 判定表的关键口径（与 docs/对客验收手册.md §6 同口径）
  const guideText = CONTRACT_TYPE_GUIDE.map((row) => row.when).join(' / ');
  assert.ok(guideText.includes('下采购单'), '采购合同要点明「会下采购单」');
  assert.ok(guideText.includes('开销售单'), '销售合同要点明「会开销售单」');
  assert.ok(guideText.includes('一个订单都不下'), '服务 / 租赁的判据是「一个订单都不下」');

  // 流转确认文案：服务 / 租赁不能说「可被采购单引用」
  const active = contractTransitions('SIGNED')[0];
  assert.ok(active, 'SIGNED 有一条出边');
  const purchaseText = transitionConfirmText('HT-1', 'SIGNED', active, 'PURCHASE');
  assert.ok(purchaseText.includes('采购单'), `采购合同应说明可被采购单引用：${purchaseText}`);
  const leaseText = transitionConfirmText('HT-1', 'SIGNED', active, 'LEASE');
  assert.ok(!leaseText.includes('可被新建采购单引用'), `租赁不能提「可被采购单引用」：${leaseText}`);
  assert.ok(leaseText.includes('服务期 / 租期'), `租赁应说明生效后算租期：${leaseText}`);
});

test('⑱ 归档前提示（E2E07-FIX-4 ④）：ACTIVE→COMPLETED 带上未关闭单据数', () => {
  const archive = contractTransitions('ACTIVE').find((item: { target: string }) => item.target === 'COMPLETED');
  assert.ok(archive, 'ACTIVE 应有「归档（COMPLETED）」这条出边');

  // ① 有在途单据 → 提示里点名张数（不硬拦，只在确认弹窗上说清）
  const warn = openOrderWarning(3, 'COMPLETED');
  assert.ok(warn.includes('3 张未关闭单据'), `提示要点名张数：${warn}`);
  const withCount = transitionConfirmText('HT-1', 'ACTIVE', archive!, 'PURCHASE', 3);
  assert.ok(withCount.includes('3 张未关闭单据'), `确认文案要带上提示：${withCount}`);

  // ② 没有在途单据 / 不是归档 → 一个字都不加（别给审计与用户塞噪声）
  assert.equal(openOrderWarning(0, 'COMPLETED'), '', '0 张不提示');
  assert.equal(openOrderWarning(3, 'TERMINATED'), '', '终止不是归档，不提示未关闭单据');
  assert.equal(openOrderWarning(null, 'COMPLETED'), '', '字段缺失按 0 处理');
  const clean = transitionConfirmText('HT-1', 'ACTIVE', archive!, 'PURCHASE', 0);
  assert.ok(!clean.includes('未关闭单据'), `0 张时不许出现提示：${clean}`);

  // ③ 口径固定在后端详情：`open_order_count`（前端不重算，只显示）
  const dto = backendSource('erp/contract_dto.rs');
  assert.ok(
    dto.includes('pub open_order_count: i64'),
    'open_order_count 必须由详情接口下发（前端据此提示）',
  );
  const model = backendSource('erp/contract_model.rs');
  assert.ok(
    model.includes('open_order_count'),
    '未关闭单据数的口径在 contract_model（前端不许自己拼 SQL 语义）',
  );
});

