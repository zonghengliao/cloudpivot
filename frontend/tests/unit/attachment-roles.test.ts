/**
 * ATT-04 单测：附件面板按 `file_role` 分栏的纯逻辑 + 词表一致性。
 *
 * # 钉死的口径（卡面验收 ①–④）
 *
 * ① 合同单据：合同扫描件 + 补充协议 → 两个分组，各有条数；
 * ② **空分组必须存在**（`count = 0`），前端据此显示「暂无 XX，点击上传」；
 * ③ 同一 role 多个文件全部落在该分组（不漏、不串组）；
 * ④ 不传 role（`file_role` 为空 / `DEFAULT` / 未知值）的文件进 DEFAULT 组。
 *
 * 另钉两条「不变量」，它们出问题时界面上不会报错、只会静默丢附件：
 *
 * - `sum(groups[].count) === items.length`；
 * - 组内换位后返回的整单顺序 = 分组顺序 × 组内顺序（后端 `PUT /sort` 要求完整 id 列表）。
 *
 * 运行：`pnpm test:unit`
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  DEFAULT_ROLE_CODE,
  emptyRoleHint,
  flattenGroups,
  groupByRole,
  moveWithinGroup,
  resolveRoleCatalog,
  type AttachmentRoleBucket,
  type RoleOption,
  type RoleTagged,
} from '../../src/components/attachment-roles-pure';

const HERE = dirname(fileURLToPath(import.meta.url));
const SRC = resolve(HERE, '../../src');

function readSource(relative: string): string {
  return readFileSync(join(SRC, relative), 'utf8');
}

/** 卡面的分组词表（ATT-04 的 6 个 + ATT-11 的 8 个），顺序 = 后端 `FileRole::GROUPS` */
const CATALOG: RoleOption[] = [
  { code: 'CONTRACT_SCAN', label: '合同扫描件' },
  { code: 'SUPPLEMENT', label: '补充协议' },
  { code: 'ACCEPTANCE', label: '验收单' },
  { code: 'INVOICE_SCAN', label: '发票影像' },
  { code: 'BANK_RECEIPT', label: '银行回单' },
  { code: 'QUOTATION', label: '报价单' },
  { code: 'CUSTOMER_PO', label: '客户PO' },
  { code: 'BIZ_LICENSE', label: '营业执照' },
  { code: 'BANK_ACCOUNT_PERMIT', label: '开户许可' },
  { code: 'QUALIFICATION', label: '资质证书' },
  { code: 'CREDIT_FILE', label: '信用资料' },
  { code: 'TRANSFER_APPROVAL', label: '调拨审批单' },
  { code: 'OPENING_EVIDENCE', label: '期初依据' },
  { code: 'DEFAULT', label: '其他附件' },
];

/** 造一条附件（只关心分组要用的字段） */
function item(file_role: string, name = `${file_role}.pdf`) {
  return { file_role, name };
}

/** 按 code 取分组（不存在直接失败，比 `?.` 更容易定位） */
function bucket<T extends RoleTagged>(
  groups: readonly AttachmentRoleBucket<T>[],
  code: string,
): AttachmentRoleBucket<T> {
  const found = groups.find((group) => group.code === code);
  assert.ok(found, `分组 ${code} 必须存在`);
  return found as AttachmentRoleBucket<T>;
}

// ==================== ① 分组切分 ====================

test('验收①：合同扫描件 + 补充协议 → 两个分组（其余是空分组）', () => {
  const items = [item('CONTRACT_SCAN', '合同扫描件.pdf'), item('SUPPLEMENT', '补充协议.pdf')];
  const groups = groupByRole(items, CATALOG);

  assert.equal(groups.length, CATALOG.length, '分组数 = 词表项数（固定清单，空分组也在）');
  assert.equal(bucket(groups, 'CONTRACT_SCAN').count, 1);
  assert.equal(bucket(groups, 'CONTRACT_SCAN').items[0].name, '合同扫描件.pdf');
  assert.equal(bucket(groups, 'SUPPLEMENT').count, 1);
  assert.equal(bucket(groups, 'SUPPLEMENT').label, '补充协议');
  // 顺序 = 词表顺序（界面分组顺序不能随数据顺序变）
  assert.deepEqual(
    groups.map((group) => group.code),
    CATALOG.map((role) => role.code),
  );
});

test('验收②：空分组保留且 count = 0（引导文案靠它）', () => {
  const groups = groupByRole([item('CONTRACT_SCAN')], CATALOG);
  for (const code of ['SUPPLEMENT', 'ACCEPTANCE', 'INVOICE_SCAN', 'BANK_RECEIPT', 'DEFAULT']) {
    assert.equal(bucket(groups, code).count, 0, `${code} 应是空分组`);
    assert.deepEqual(bucket(groups, code).items, []);
  }
  // 完全没数据时也是全量空分组（面板首次打开就是这个状态）
  const empty = groupByRole([], CATALOG);
  assert.equal(empty.length, CATALOG.length);
  assert.ok(empty.every((group) => group.count === 0));
});

test('验收③：同一 role 多个文件全部落在该分组，且保持传入顺序', () => {
  const items = [
    item('CONTRACT_SCAN', '第一页.pdf'),
    item('SUPPLEMENT', '补充一.pdf'),
    item('CONTRACT_SCAN', '第二页.pdf'),
    item('SUPPLEMENT', '补充二.pdf'),
  ];
  const groups = groupByRole(items, CATALOG);
  assert.deepEqual(
    bucket(groups, 'CONTRACT_SCAN').items.map((row) => row.name),
    ['第一页.pdf', '第二页.pdf'],
    '同组内保持传入顺序（= 当前排序口径）',
  );
  assert.deepEqual(
    bucket(groups, 'SUPPLEMENT').items.map((row) => row.name),
    ['补充一.pdf', '补充二.pdf'],
  );
  assert.equal(
    groups.reduce((sum, group) => sum + group.count, 0),
    items.length,
    '不变式：每条附件恰好落进一个分组',
  );
});

test('验收④：不传 role / 未知 role / 兼容 role → 都有明确去处，绝不丢附件', () => {
  const items = [
    item('DEFAULT', '不传角色的.pdf'),
    item('', '空字符串.pdf'),
    item('NOT_A_ROLE', '后端未知值.pdf'),
    item('CONTRACT_SCAN', '扫描件.pdf'),
  ];
  const groups = groupByRole(items, CATALOG);
  const fallback = bucket(groups, DEFAULT_ROLE_CODE);
  assert.equal(fallback.count, 3, '空值 / 未知值都并入其他附件');
  assert.deepEqual(
    fallback.items.map((row) => row.name),
    ['不传角色的.pdf', '空字符串.pdf', '后端未知值.pdf'],
  );
  assert.equal(groups.reduce((sum, group) => sum + group.count, 0), 4);
});

test('词表里没有 DEFAULT 时，未知 role 被丢弃而不是造一个无标题分组', () => {
  const catalog = CATALOG.filter((role) => role.code !== DEFAULT_ROLE_CODE);
  const groups = groupByRole([item('NOT_A_ROLE'), item('CONTRACT_SCAN')], catalog);
  assert.equal(groups.length, catalog.length);
  assert.ok(groups.every((group) => group.code !== 'NOT_A_ROLE'));
  assert.equal(groups.reduce((sum, group) => sum + group.count, 0), 1);
});

test('兼容 role SIGN_RECEIPT：接口给了分组就单独成组，不并进验收单', () => {
  const catalog = [...CATALOG, { code: 'SIGN_RECEIPT', label: '签收单' }];
  const groups = groupByRole([item('SIGN_RECEIPT'), item('ACCEPTANCE')], catalog);
  assert.equal(bucket(groups, 'SIGN_RECEIPT').count, 1);
  assert.equal(bucket(groups, 'ACCEPTANCE').count, 1);
  assert.equal(groups.length, catalog.length);
});

// ==================== 词表解析 ====================

test('resolveRoleCatalog：接口下发优先，老实例回落本地词表', () => {
  const backend: RoleOption[] = [{ code: 'CONTRACT_SCAN', label: '合同扫描件（后端）' }];
  assert.deepEqual(resolveRoleCatalog(backend, CATALOG), backend, '有后端分组就用后端的');

  assert.deepEqual(resolveRoleCatalog([], CATALOG), CATALOG, '空数组回落本地');
  assert.deepEqual(resolveRoleCatalog(null, CATALOG), CATALOG, 'null 回落本地');
  assert.deepEqual(resolveRoleCatalog(undefined, CATALOG), CATALOG, 'undefined 回落本地');

  // 后端漏了 label 时用 code 顶替（宁可显示英文，也不能空标题）
  assert.deepEqual(resolveRoleCatalog([{ code: 'DEFAULT', label: '' }], CATALOG), [
    { code: 'DEFAULT', label: 'DEFAULT' },
  ]);
});

test('空分组引导文案：暂无 XX，点击上传（卡面验收②）', () => {
  assert.equal(emptyRoleHint('合同扫描件'), '暂无合同扫描件，点击上传');
  assert.equal(emptyRoleHint('补充协议'), '暂无补充协议，点击上传');
  assert.equal(emptyRoleHint('其他附件'), '暂无其他附件，点击上传');
});

// ==================== 组内换位 → 整单顺序 ====================

test('moveWithinGroup：组内上移 / 下移后，整单顺序 = 分组顺序 × 组内顺序', () => {
  const items = [
    item('CONTRACT_SCAN', 'A1'),
    item('SUPPLEMENT', 'S1'),
    item('CONTRACT_SCAN', 'A2'),
    item('SUPPLEMENT', 'S2'),
  ];
  // 合同组内：A1(0) 下移 → A2, A1；补充组不受影响
  const moved = moveWithinGroup(items, CATALOG, 'CONTRACT_SCAN', 0, 1);
  assert.deepEqual(
    moved.map((row) => row.name),
    ['A2', 'A1', 'S1', 'S2'],
    '整单顺序按分组词表拼接（合同组在前）',
  );
  // 补充组内下移同理
  const moved2 = moveWithinGroup(items, CATALOG, 'SUPPLEMENT', 1, 0);
  assert.deepEqual(
    moved2.map((row) => row.name),
    ['A1', 'A2', 'S2', 'S1'],
  );
  // 不改入参（面板靠它做乐观更新 + 失败回滚）
  assert.deepEqual(
    items.map((row) => row.name),
    ['A1', 'S1', 'A2', 'S2'],
  );
});

test('moveWithinGroup：越界 / 相等 / 未知分组一律原样返回', () => {
  const items = [item('CONTRACT_SCAN', 'A1'), item('CONTRACT_SCAN', 'A2')];
  assert.deepEqual(
    moveWithinGroup(items, CATALOG, 'CONTRACT_SCAN', 0, 0).map((row) => row.name),
    ['A1', 'A2'],
  );
  assert.deepEqual(
    moveWithinGroup(items, CATALOG, 'CONTRACT_SCAN', 0, 5).map((row) => row.name),
    ['A1', 'A2'],
  );
  assert.deepEqual(
    moveWithinGroup(items, CATALOG, 'NOT_A_ROLE', 0, 1).map((row) => row.name),
    ['A1', 'A2'],
  );
  assert.deepEqual(moveWithinGroup([], CATALOG, 'CONTRACT_SCAN', 0, 1), []);
});

test('flattenGroups：平铺顺序与界面顺序一致（预览左右切换靠它）', () => {
  const groups = groupByRole(
    [item('SUPPLEMENT', 'S1'), item('CONTRACT_SCAN', 'A1'), item('BANK_RECEIPT', 'B1')],
    CATALOG,
  );
  assert.deepEqual(
    flattenGroups(groups).map((row) => row.name),
    ['A1', 'S1', 'B1'],
    '预览顺序按分组排，不按接口原始顺序',
  );
});

// ==================== 源码级契约 ====================

test('面板：分组清单来自接口、用纯函数分组与整单重排', () => {
  const panel = readSource('views/business/erp/shared/DocAttachmentPanel.vue');
  assert.ok(panel.includes('resolveRoleCatalog'), '必须用 resolveRoleCatalog 取分组口径');
  assert.ok(panel.includes('groupByRole'), '必须用 groupByRole 分组（不许模板里手写过滤）');
  assert.ok(panel.includes('emptyRoleHint'), '空分组文案必须来自 emptyRoleHint');
  assert.ok(panel.includes('moveWithinGroup'), '组内换位必须换算回整单顺序');
  assert.ok(panel.includes('flattenGroups'), '预览顺序必须用 flattenGroups');
  assert.ok(panel.includes('backendRoles'), '必须接住接口下发的 roles');
  // 每个分组都要有独立上传入口（支持多个 role 同时上传）
  assert.ok(panel.includes('-upload-${group.code}'), '分组头必须有上传按钮');
  assert.ok(panel.includes('-empty-${group.code}'), '空分组引导必须可点上传');
  assert.ok(panel.includes('multiple'), '一次可选多个文件');
});

test('接口词表：FILE_ROLE_OPTIONS = 卡面 14 项，且出参带 roles', () => {
  const api = readSource('api/business/erp/attachment.ts');
  // 14 个 code 与卡面顺序一致（Rust 侧 file_roles_match_frontend_vocabulary 再钉一遍）
  const codes = [...api.matchAll(/value: '([A-Z_]+)', label: '([^']+)'/g)].map((match) => [
    match[1],
    match[2],
  ]);
  assert.deepEqual(codes, CATALOG.map((role) => [role.code, role.label]));
  assert.ok(api.includes('roles: DocAttachmentRoleGroup[]'), 'list 出参必须声明 roles');
  assert.ok(api.includes("DEFAULT: 'DEFAULT'") || api.includes("'DEFAULT'"), '必须有 DEFAULT 角色');
  assert.ok(
    !api.includes("value: 'OTHER'") && !api.includes("value: 'INVOICE'"),
    '老拼写不该出现在上传下拉里（后端会归一，但下拉不该提供）',
  );
});
