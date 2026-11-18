/**
 * 供应商准入 / 黑名单页面纯逻辑单测（M4-14 后端 + 本卡前端）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖、无 DOM）。
 *
 * # 钉住哪几条
 *
 * | 组 | 钉住什么 | 翻车后果 |
 * |----|----------|----------|
 * | ① | 分页入参：空串 / 空关键字**整键不传**，超长关键字**不截断** | `?keyword=` 查出 0 行；截断后结果「看着有、其实不全」 |
 * | ② | 闭集与中文名与后端迁移逐字一致（3 类型 / 3 状态 / 4 等级） | 后端加一个值、前端标签回落成「未知」，界面静默变瞎 |
 * | ③ | 理由字数**按字符**算（中文 1 字 = 1，emoji 也 1） | 用 `.length` 会让「够 5 字」的单被本地拦下，或反之放过去后端 400 |
 * | ④ | 可改 / 可提交**只读后端 `editable` / `submittable`** | 前端自己推状态机 → 与后端状态机分叉，按钮能点但必 409 |
 * | ⑤ | 「通过后」读 `target_supplier_status`，**不按类型重推** | 拉黑单被显示成「启用」这种致命误导 |
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';

import type { AccessRow } from '@/api/business/erp/supplier-access';
import {
  GRADE_OPTIONS,
  MAX_KEYWORD,
  MIN_REASON_CHARS,
  PAGE_SIZE,
  STATUS_OPTIONS,
  TYPE_OPTIONS,
  accessStatusLabel,
  accessTypeLabel,
  applicantText,
  buildAccessQuery,
  canEditRow,
  canSubmitRow,
  decidedText,
  detailTitle,
  emptyAccessFilter,
  emptyListText,
  keywordWarning,
  reasonError,
  reasonLength,
  statusTheme,
  submitBlockedTip,
  supplierStateLabel,
  targetStateLabel,
  timeText,
  typeHint,
  typeTheme,
} from '@/views/business/erp/supplier-access/supplier-access-logic';

/** 一行完整申请单（只覆盖被测字段，其余给「合法的默认值」） */
function row(overrides: Partial<AccessRow> = {}): AccessRow {
  return {
    id: '00000000-0000-0000-0000-000000000001',
    access_no: 'SAC-20261118-000001',
    supplier_id: '00000000-0000-0000-0000-0000000000aa',
    supplier_code: 'SUP-0001',
    supplier_name: '北京示例供应商有限公司',
    access_type: 'ADMIT',
    access_type_label: '准入',
    status: 'DRAFT',
    status_label: '拟稿',
    reason: null,
    grade_snapshot: null,
    apply_by: null,
    apply_by_name: null,
    approval_instance_id: null,
    decided_by: null,
    decided_by_name: null,
    decided_at: null,
    remark: null,
    created_at: '2026-11-18T09:30:00+08:00',
    supplier_status: 1,
    supplier_status_label: '启用',
    target_supplier_status: 1,
    editable: true,
    submittable: false,
    ...overrides,
  };
}

// ==================== ① 分页入参 ====================

test('分页入参：空筛选不带键，page/page_size 归一成正整数', () => {
  const query = buildAccessQuery(emptyAccessFilter(), 1, PAGE_SIZE);
  assert.deepEqual(query, { page: 1, page_size: PAGE_SIZE });
  assert.equal('keyword' in query, false, '空关键字必须整键缺席（?keyword= 会查出 0 行）');
  assert.equal('status' in query, false);
  assert.equal('access_type' in query, false);

  const bad = buildAccessQuery(emptyAccessFilter(), 0, 0);
  assert.equal(bad.page, 1, 'page=0 归一成 1');
  assert.equal(bad.page_size, PAGE_SIZE, 'page_size=0 回落默认 20');
});

test('分页入参：关键字 trim 后传；状态/类型原样透传', () => {
  const query = buildAccessQuery(
    { keyword: '  示例供应商  ', status: 'SUBMITTED', access_type: 'BLACKLIST' },
    3,
    50,
  );
  assert.deepEqual(query, {
    page: 3,
    page_size: 50,
    keyword: '示例供应商',
    status: 'SUBMITTED',
    access_type: 'BLACKLIST',
  });
});

test('分页入参：超长关键字不截断，只给提示', () => {
  const long = 'x'.repeat(MAX_KEYWORD + 5);
  const query = buildAccessQuery({ keyword: long, status: '', access_type: '' }, 1, PAGE_SIZE);
  assert.equal(query.keyword, long, '截断会「看着有结果、其实少了一半」——交给后端 400');
  assert.match(keywordWarning(long), /超过后端上限/);
  assert.equal(keywordWarning('x'.repeat(MAX_KEYWORD)), '', '刚好等于上限不提示');
});

// ==================== ② 闭集与中文名（与迁移 / model 常量逐字一致） ====================

test('类型闭集与中文名与 supplier_access_model 逐字一致', () => {
  assert.deepEqual(
    TYPE_OPTIONS.map((item) => item.value),
    ['ADMIT', 'BLACKLIST', 'RELEASE'],
    '顺序即下拉顺序，必须与后端 ACCESS_TYPES 一致',
  );
  assert.equal(accessTypeLabel('ADMIT'), '准入');
  assert.equal(accessTypeLabel('BLACKLIST'), '黑名单');
  assert.equal(accessTypeLabel('RELEASE'), '解除黑名单');
  assert.equal(accessTypeLabel('WHATEVER'), '未知类型', '未知类型不猜成准入');
});

test('状态闭集只有三个值（没有 REJECTED：驳回是打回 DRAFT）', () => {
  assert.deepEqual(STATUS_OPTIONS.map((item) => item.value), ['DRAFT', 'SUBMITTED', 'APPROVED']);
  assert.equal(accessStatusLabel('DRAFT'), '拟稿');
  assert.equal(accessStatusLabel('SUBMITTED'), '审批中');
  assert.equal(accessStatusLabel('APPROVED'), '已生效');
  assert.equal(accessStatusLabel('REJECTED'), '未知状态', '不存在「已驳回」状态');
});

test('等级快照闭集 A/B/C/D', () => {
  assert.deepEqual(GRADE_OPTIONS.map((item) => item.value), ['A', 'B', 'C', 'D']);
});

test('状态 / 类型标签主题：黑名单红、恢复方向绿', () => {
  assert.equal(statusTheme('DRAFT'), 'default');
  assert.equal(statusTheme('SUBMITTED'), 'warning');
  assert.equal(statusTheme('APPROVED'), 'success');
  assert.equal(typeTheme('BLACKLIST'), 'danger');
  assert.equal(typeTheme('ADMIT'), 'success');
  assert.equal(typeTheme('RELEASE'), 'success');
  assert.equal(typeTheme('WHATEVER'), 'default');
});

// ==================== ③ 理由字数按字符算 ====================

test('理由字数按字符：中文 1 字算 1，emoji 也算 1，前后空白忽略', () => {
  assert.equal(reasonLength('供应商失联'), 5);
  assert.equal(reasonLength('  供应商失联  '), 5);
  assert.equal(reasonLength(''), 0);
  assert.equal(reasonLength(null), 0);
  assert.equal(reasonLength('😀😀😀😀'), 4, 'emoji 的 UTF-16 length 是 8，但后端 chars() 是 4');
  assert.equal(MIN_REASON_CHARS, 5, '与库层 ck_biz_sa_reason_on_submit 的同一个 5');
});

test('提交理由校验：空 / 太短 / 够长三条路径', () => {
  assert.match(reasonError('') ?? '', /必须写申请理由/);
  assert.match(reasonError(null) ?? '', /至少 5 个字/);
  assert.match(reasonError('四个汉字') ?? '', /当前 4 个/);
  assert.equal(reasonError('供应商失联'), null);
  assert.equal(reasonError('😀😀😀😀😀'), null, '5 个 emoji = 5 个字符，后端认');
});

// ==================== ④ 可改 / 可提交只读后端字段 ====================

test('可改 / 可提交只认后端 editable / submittable', () => {
  assert.equal(canEditRow(row({ status: 'DRAFT', editable: true })), true);
  assert.equal(canEditRow(row({ status: 'SUBMITTED', editable: false })), false);
  assert.equal(
    canSubmitRow(row({ status: 'DRAFT', reason: '供应商失联', editable: true, submittable: true })),
    true,
  );
  assert.equal(canSubmitRow(row({ status: 'DRAFT', submittable: false })), false);
  // 前端**不**自己拿 status + 理由长度重算：后端说不行就是不行
  assert.equal(
    canSubmitRow(row({ status: 'DRAFT', reason: '供应商失联超过五字', submittable: false })),
    false,
    'submittable=false 时不看理由长度',
  );
});

test('提交按钮的置灰原因：非草稿说状态、草稿说字数', () => {
  assert.equal(submitBlockedTip(row({ status: 'DRAFT', submittable: true })), '', '能提交就不给提示');
  assert.match(submitBlockedTip(row({ status: 'SUBMITTED', submittable: false })), /只有草稿能提交（当前：审批中）/);
  assert.match(submitBlockedTip(row({ status: 'DRAFT', reason: '短', submittable: false })), /理由至少 5 个字/);
});

// ==================== ⑤ 「通过后」与展示小工具 ====================

test('「通过后」读 target_supplier_status，不按类型重推', () => {
  assert.equal(targetStateLabel(row({ access_type: 'BLACKLIST', target_supplier_status: 0 })), '停用（不可下单）');
  assert.equal(targetStateLabel(row({ access_type: 'ADMIT', target_supplier_status: 1 })), '启用（可下单）');
  // 后端给了「停用」就照它显示，即便类型是准入（前端不重推，重推迟早与后端分叉）
  assert.equal(targetStateLabel(row({ access_type: 'ADMIT', target_supplier_status: 0 })), '停用（不可下单）');
  assert.equal(supplierStateLabel(1), '启用');
  assert.equal(supplierStateLabel(0), '停用');
});

test('申请人 / 结论 / 时间展示', () => {
  assert.equal(applicantText(row({ apply_by_name: '张三' })), '张三');
  assert.equal(applicantText(row({ apply_by_name: null })), '—', '没有姓名快照不显示 uuid');
  assert.equal(decidedText(row({ decided_at: null })), '—');
  assert.equal(
    decidedText(row({ decided_at: '2026-11-18T10:05:00+08:00', decided_by_name: '李四' })),
    '李四 · 2026-11-18 10:05',
  );
  assert.equal(
    decidedText(row({ decided_at: '2026-11-18T10:05:00+08:00', decided_by_name: null })),
    '（已删除用户） · 2026-11-18 10:05',
  );
  assert.equal(timeText('2026-11-18T09:30:00+08:00'), '2026-11-18 09:30');
  assert.equal(timeText(null), '—');
  assert.equal(timeText('不是一个时间'), '不是一个时间', '解析不了就原样给，不吞成空');
});

test('空态文案按「有没有筛选」分两种', () => {
  assert.match(emptyListText(emptyAccessFilter()), /还没有准入 \/ 黑名单申请单/);
  assert.match(emptyListText({ keyword: 'x', status: '', access_type: '' }), /没有符合条件的申请单/);
  assert.match(emptyListText({ keyword: '', status: 'APPROVED', access_type: '' }), /没有符合条件的申请单/);
});

test('类型说明与详情标题', () => {
  assert.match(typeHint('BLACKLIST'), /通过后停用/);
  assert.match(typeHint('RELEASE'), /恢复启用/);
  assert.match(typeHint(''), /先选类型/);
  assert.equal(detailTitle(null), '申请单详情');
  assert.equal(detailTitle(row()), 'SAC-20261118-000001 · 准入');
  assert.equal(
    detailTitle(row({ access_type_label: '', access_type: 'BLACKLIST' })),
    'SAC-20261118-000001 · 黑名单',
    '后端没给中文名时按类型码兜底',
  );
});
