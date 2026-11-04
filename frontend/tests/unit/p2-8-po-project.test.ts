/**
 * P2-8-F1 单测：采购单「所属项目」—— 表单状态 / 请求体 / 显示回退 / 下拉兜底。
 *
 * 父卡 `t_b87398c8` 已把后端契约冻住（V0237 + DTO + 校验），本文件只钉前端口径。
 *
 * # 三条必须钉死的坑
 *
 * 1. **改单是整单覆盖**：不传 `project_id` = 清空归属。所以 `buildPurchasePayload` 必须
 *    **永远**带这个键（空串 → `null`）。这条是防「编辑一次就把项目洗掉」的回归钉 ——
 *    如果哪天有人觉得「空值就别发了」，编辑任何一张带项目的单都会静默清空。
 * 2. **一次选择不进记忆**：`MEMORY_FIELDS` 里不许有 `project_id`，否则下一张单会默默挂到
 *    上一张的项目上。
 * 3. **显示永不落 uuid**：`project_name` 是后端实时 JOIN 的现名；项目软删后只剩 `project_id`，
 *    此时显示「（已删除项目）」而不是 `0f8a3c1d…`（与 P2-7-F1 的供应商名口径同一条）。
 *
 * # 为什么还要读 `.vue` 源码
 *
 * `vue-tsc` 抓不到「列没加进 columns」「插槽里又偷偷用了 shortId」「下拉没挂兜底项」这类回归。
 * 本仓既有姿势是「源码即契约」（见 `p2-7-supplier-display-name.test.ts`），这里沿用。
 *
 * 运行：`pnpm test:unit`
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join, resolve } from 'node:path';

import {
  DELETED_PROJECT_TEXT,
  NO_PROJECT_PLACEHOLDER,
  buildPurchasePayload,
  detailToForm,
  emptyPurchaseForm,
  projectDisplayName,
  projectOptionLabel,
  projectSelectOptions,
  type ProjectOptionLike,
} from '../../src/views/business/erp/purchase/purchase-logic.ts';

/** 一根真 uuid（固定值，便于断言「输出里不含它、也不含它的短号」） */
const PROJECT_ID = '7b1e4c2a-8f3d-4a51-b6c7-9d0e1f2a3b4c';
/** 另一根 uuid（不在下拉清单里的「不可见项目」） */
const HIDDEN_PROJECT_ID = 'c4d5e6f7-0a1b-4c2d-8e3f-4a5b6c7d8e9f';

const SOURCE = readFileSync(
  join(resolve(process.cwd()), 'src/views/business/erp/purchase/index.vue'),
  'utf8',
);

/** 详情抽屉模板区（断言只在抽屉里做，别命中列表列） */
const DRAWER_START = SOURCE.indexOf('<t-drawer');
assert.notEqual(DRAWER_START, -1, '找不到详情抽屉 <t-drawer>');
const DRAWER_END = SOURCE.indexOf('</t-drawer>', DRAWER_START);
assert.notEqual(DRAWER_END, -1, '找不到 </t-drawer>');
const DRAWER = SOURCE.slice(DRAWER_START, DRAWER_END);

/** uuid 短号形态（`shortId()` = 前 8 位 + `…`） */
const SHORT_ID_PATTERN = /[0-9a-f]{8}…/i;

/** 造一张「够 detailToForm 用」的单头（只给会被读到的字段，其余当 undefined） */
function rowOf(projectId: string | null): Record<string, unknown> {
  return { id: 'po-1', project_id: projectId, supplier_id: 'sup-1', warehouse_id: 'wh-1', order_date: '2026-11-05' };
}

/** 调 `detailToForm`：入参是真 `PurchaseOrderRow` 的弱化版，类型上做一次显式收口 */
function formOfProject(projectId: string | null): ReturnType<typeof detailToForm> {
  const order = rowOf(projectId) as unknown as Parameters<typeof detailToForm>[0];
  return detailToForm(order, []);
}

// ==================== ① 表单状态：空串 = 未选 ====================

test('空表单 `project_id` 是空串（= 未选 / 不归集），不是 undefined 也不是 null', () => {
  const form = emptyPurchaseForm();
  assert.equal(form.project_id, '');
  assert.equal(typeof form.project_id, 'string');
});

test('编辑回填：`project_id = X` → 表单 `X`；`null` / 缺字段 → 空串', () => {
  assert.equal(formOfProject(PROJECT_ID).project_id, PROJECT_ID);
  assert.equal(formOfProject(null).project_id, '');
  // 后端老版本 / 中间层漏字段时也不能变成 undefined（否则下拉显示成「未选」）
  const order = { id: 'po-2' } as unknown as Parameters<typeof detailToForm>[0];
  assert.equal(detailToForm(order, []).project_id, '');
});

// ==================== ② 请求体：整单覆盖口径（回归钉） ====================

test('`buildPurchasePayload` 带项目 → `project_id` 是 uuid 原文', () => {
  const form = { ...emptyPurchaseForm(), project_id: PROJECT_ID };
  assert.equal(buildPurchasePayload(form).project_id, PROJECT_ID);
});

test('`buildPurchasePayload` 未选项目 → `project_id: null`，**键必须存在**（改单不传即清空）', () => {
  const form = { ...emptyPurchaseForm(), project_id: '' };
  const payload = buildPurchasePayload(form);
  assert.equal(payload.project_id, null);
  // ★ 这条是钉：整单覆盖下「缺席」和「null」语义相同，但前端一旦改成「空就不带这个键」，
  //   字段拼接逻辑（asParams 之外的 body 组装）随时可能把键也删掉 —— 属性存在才算钉住。
  assert.equal(Object.prototype.hasOwnProperty.call(payload, 'project_id'), true, 'project_id 键不许缺席');
});

test('清空项目再保存 = 显式 `null`（不是空串、不是 uuid）', () => {
  const edited = { ...formOfProject(PROJECT_ID) };
  edited.project_id = '';
  assert.equal(buildPurchasePayload(edited).project_id, null);
});

// ==================== ③ 显示回退：名字 → （已删除项目） → — ====================

test('有 `project_name` → 显示名字（后端实时 JOIN 的现名，项目改名后跟着变）', () => {
  assert.equal(projectDisplayName(PROJECT_ID, '亦庄智造产线改造'), '亦庄智造产线改造');
});

test('只有 `project_id`、没有名字 → 「（已删除项目）」（项目被软删）', () => {
  assert.equal(projectDisplayName(PROJECT_ID, null), DELETED_PROJECT_TEXT);
  assert.equal(projectDisplayName(PROJECT_ID, ''), DELETED_PROJECT_TEXT);
  assert.equal(projectDisplayName(PROJECT_ID, '   '), DELETED_PROJECT_TEXT);
});

test('id 与名字都没有 → `—`（未归集项目，不留白）', () => {
  assert.equal(projectDisplayName(null, null), '—');
  assert.equal(projectDisplayName(undefined, undefined), '—');
  assert.equal(projectDisplayName('', null), '—');
});

test('任何输入组合下都不出现 uuid / uuid 短号（禁落 uuid 的回归闸门）', () => {
  const cases: Array<[string | null | undefined, string | null | undefined]> = [
    [PROJECT_ID, '亦庄智造产线改造'],
    [PROJECT_ID, null],
    [PROJECT_ID, ''],
    [null, null],
    ['', ''],
  ];
  for (const [id, name] of cases) {
    const display = projectDisplayName(id, name);
    assert.equal(display.includes(PROJECT_ID), false, `输出里出现了完整 uuid：${display}`);
    assert.equal(display.includes(PROJECT_ID.slice(0, 8)), false, `输出里出现了 uuid 前 8 位：${display}`);
    assert.equal(SHORT_ID_PATTERN.test(display), false, `输出里出现了 uuid 短号：${display}`);
  }
});

// ==================== ④ 下拉：文案 + 归档不筛 + 选中值兜底 ====================

const ACTIVE_PROJECT: ProjectOptionLike = { id: PROJECT_ID, name: '亦庄智造产线改造', code: 'PRJ-2026-001', status: 'ACTIVE' };
const ARCHIVED_PROJECT: ProjectOptionLike = { id: 'a1b2c3d4-0000-4000-8000-000000000001', name: '2025 老产线', code: 'PRJ-2025-009', status: 'ARCHIVED' };

test('选项文案 = `名称（编码）`；**归档项目加后缀但不筛掉**', () => {
  assert.equal(projectOptionLabel(ACTIVE_PROJECT), '亦庄智造产线改造（PRJ-2026-001）');
  assert.equal(projectOptionLabel(ARCHIVED_PROJECT), '2025 老产线（PRJ-2025-009）（已归档）');
  // 缺 status 的项目（后端老版本）按 ACTIVE 处理，不加后缀
  assert.equal(projectOptionLabel({ ...ACTIVE_PROJECT, status: undefined }), '亦庄智造产线改造（PRJ-2026-001）');

  const options = projectSelectOptions([ACTIVE_PROJECT, ARCHIVED_PROJECT], { id: '' });
  assert.equal(options.length, 2, '归档项目不能被筛掉（后端允许挂归档项目）');
});

test('选中值在清单里 → 原样返回，不重复插兜底项', () => {
  const options = projectSelectOptions([ACTIVE_PROJECT, ARCHIVED_PROJECT], { id: ARCHIVED_PROJECT.id });
  assert.equal(options.length, 2);
  assert.equal(options.filter((option) => option.value === ARCHIVED_PROJECT.id).length, 1);
});

test('选中值**不在**清单里（未参与 / 已删）→ 补一条兜底项，别让选择器静默变空', () => {
  const visible = projectSelectOptions([ACTIVE_PROJECT], { id: HIDDEN_PROJECT_ID, name: '我没参与的项目' });
  assert.equal(visible.length, 2);
  assert.equal(visible[0].value, HIDDEN_PROJECT_ID, '兜底项放第一位（编辑老单一眼看到当前值）');
  assert.match(visible[0].label, /我没参与的项目/);
  assert.match(visible[0].label, /当前所属项目/);

  // 连名字都没有（项目已删）→ 用「（已删除项目）」当兜底文案，仍然不落 uuid
  const deleted = projectSelectOptions([], { id: HIDDEN_PROJECT_ID, name: null });
  assert.equal(deleted.length, 1);
  assert.equal(deleted[0].label, `${DELETED_PROJECT_TEXT}（当前所属项目）`);
  assert.equal(deleted[0].label.includes(HIDDEN_PROJECT_ID), false);
});

test('未选项目（空串）→ 不插兜底项（不能凭空给一个「空 uuid」选项）', () => {
  assert.equal(projectSelectOptions([ACTIVE_PROJECT], { id: '' }).length, 1);
});

// ==================== ⑤ 源码契约：列 / 插槽 / 详情 / 下拉 / 记忆 ====================

test('列表 columns 在「供应商」列之后加了「项目」列（宽 160 + 省略）', () => {
  const supplierIdx = SOURCE.indexOf("{ colKey: 'supplier_id'");
  const projectIdx = SOURCE.indexOf("{ colKey: 'project_name'");
  assert.notEqual(supplierIdx, -1, '找不到供应商列');
  assert.notEqual(projectIdx, -1, '找不到项目列（colKey 必须是 project_name，插槽才对得上）');
  assert.ok(supplierIdx < projectIdx, '项目列应在供应商列之后');
  const colLine = SOURCE.slice(projectIdx, SOURCE.indexOf('\n', projectIdx));
  assert.match(colLine, /title: '项目'/);
  assert.match(colLine, /width: 160/);
  assert.match(colLine, /ellipsis: true/);
});

test('列表插槽走 `projectDisplayName(row.project_id, row.project_name)`，不许出现 shortId / uuid 短号', () => {
  const slot = /<template #project_name="\{ row \}">([\s\S]*?)<\/template>/.exec(SOURCE);
  assert.notEqual(slot, null, '找不到 #project_name 插槽');
  const body = slot![1];
  assert.match(body, /projectDisplayName\(row\.project_id, row\.project_name\)/);
  assert.equal(/shortId\s*\(/.test(body), false, '项目列里出现了 shortId 兜底（会显示 uuid 短号）');
  assert.equal(/row\.project_id\s*\}\}/.test(body), false, '项目列里直接渲染了 project_id');
  assert.match(body, /purchase-project-\$\{row\.order_no\}/);
});

test('详情抽屉有「所属项目」一行，绑 `projectDisplayName(detail.project_id, detail.project_name)`', () => {
  const idx = DRAWER.indexOf('所属项目');
  assert.notEqual(idx, -1, '详情抽屉缺「所属项目」栏');
  const block = DRAWER.slice(idx, idx + 700);
  assert.match(block, /projectDisplayName\(detail\.project_id, detail\.project_name\)/);
  assert.match(block, /data-testid="purchase-detail-project"/);
  assert.equal(/shortId\s*\(/.test(DRAWER.slice(idx, DRAWER.indexOf('下单日期', idx))), false, '详情项目栏不许用 shortId');
});

test('表单下拉：`purchase-project`、clearable + filterable、placeholder = NO_PROJECT_PLACEHOLDER', () => {
  const m = /<t-select\s+([\s\S]*?)data-testid="purchase-project"\s*\/>/.exec(SOURCE);
  assert.notEqual(m, null, '找不到「所属项目」下拉（data-testid=purchase-project）');
  const attrs = m![1];
  assert.match(attrs, /v-model="form\.project_id"/);
  assert.match(attrs, /:options="projectOptions"/);
  assert.match(attrs, /:placeholder="NO_PROJECT_PLACEHOLDER"/);
  assert.match(attrs, /clearable/);
  assert.match(attrs, /filterable/);
  assert.equal(NO_PROJECT_PLACEHOLDER, '可空：不归集项目');
});

test('表单下拉接兜底选项 `projectSelectOptions`，数据源 `pageProjects` 失败不挡页面', () => {
  assert.match(SOURCE, /const projectOptions = computed\(\(\) =>\s*projectSelectOptions\(/);
  assert.match(SOURCE, /pageProjects\(\{ page: 1, page_size: 100 \}\)\.catch\(\(\) => null\)/);
  assert.match(SOURCE, /projects\.value = projectPage\?\.list \?\? \[\]/);
});

test('`project_id` 不进表单记忆（否则下一张单会默默挂错项目）', () => {
  const start = SOURCE.indexOf('const MEMORY_FIELDS');
  assert.notEqual(start, -1, '找不到 MEMORY_FIELDS');
  const end = SOURCE.indexOf('];', start);
  const block = SOURCE.slice(start, end);
  assert.equal(/project_id/.test(block), false, 'MEMORY_FIELDS 里出现了 project_id');
  assert.equal(/project/.test(block), false, 'MEMORY_FIELDS 里出现了任何项目字段');
});
