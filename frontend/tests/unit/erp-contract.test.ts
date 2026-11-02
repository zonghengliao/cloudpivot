/**
 * 合同管理页（A5）单测：14 条，全部跑在无 DOM 的 `tsx --test` 里。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`）。
 *
 * # 为什么测 `.ts` + 读源码，而不是渲染 `.vue`
 *
 * 本仓没有 happy-dom/jsdom（零新依赖），`tests/unit` 里也没有组件渲染的先例。
 * 但这些用例真正要钉住的是**口径**：
 * - 5 个状态的文案与配色（`constants/status-map.ts`，页面只调它）；
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
  CONTRACT_EDGES,
  CONTRACT_PAGE_SIZE,
  CONTRACT_REASON_EDGES,
  CONTRACT_STATUSES,
  buildContractQuery,
  buildCreatePayload,
  buildStatusPayload,
  buildUpdatePayload,
  canDeleteContract,
  canEditContract,
  contractAmountText,
  contractEmptyState,
  contractFormFromDetail,
  contractListState,
  contractPeriodText,
  contractStatusLabel,
  contractStatusTheme,
  contractTransitions,
  contractTypeLabel,
  contractTypeOptions,
  emptyContractFilter,
  emptyContractForm,
  isContractFiltered,
  needsReason,
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

test('⑥ 状态徽标：5 个状态的文案与配色（照卡面色表 + 后端 status_label）', () => {
  const expected = [
    ['DRAFT', '草稿', 'default'],
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

  // ---- 按钮矩阵
  assert.deepEqual(
    contractTransitions('DRAFT').map((item) => [item.label, item.target]),
    [['登记签订', 'SIGNED']],
    'DRAFT 只有一条边：登记签订（卡面的「生效」在 SIGNED 之后）',
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

  // 两条终止边必须带原因，其余边不带
  assert.equal(needsReason('SIGNED', 'TERMINATED'), true);
  assert.equal(needsReason('ACTIVE', 'TERMINATED'), true);
  assert.equal(needsReason('ACTIVE', 'COMPLETED'), false);
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
    contractTransitions('DRAFT', []).map((item) => item.target),
    ['SIGNED'],
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

test('⑭ 编辑回填与类型下拉：详情 → 表单字段齐全，类型只放开「采购合同」', () => {
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

  // 类型下拉：PURCHASE 可选，其余置灰并标注未开放
  const options = contractTypeOptions();
  assert.equal(options.length, 4, '后端白名单 4 个值都列出来（置灰而不是消失）');
  assert.deepEqual(
    options.filter((item) => !item.disabled).map((item) => item.value),
    ['PURCHASE'],
  );
  assert.ok(options.filter((item) => item.disabled).every((item) => item.label.includes('未开放')));
  assert.equal(contractTypeLabel('PURCHASE'), '采购合同');
  assert.equal(contractTypeLabel('SALES'), '销售合同');
  assert.equal(contractTypeLabel(null), '—');
});
