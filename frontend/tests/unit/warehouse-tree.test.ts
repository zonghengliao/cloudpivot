/**
 * `warehouse-tree.ts` 单测（W4a）：仓库树的折叠 / 搜索 / 叶子约束 / 表单请求体。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * 为什么测 .ts 不测 .vue：页面里「哪些节点展开、非叶子为什么不能建库位、PUT 到底带了哪些字段」
 * 全在 `warehouse-tree.ts`，`.vue` 只渲染。这些判断一旦漂移，`vue-tsc` 和 e2e 都看不出来
 * （e2e 只看得见「点了之后界面长什么样」）。
 *
 * 断言里的数字 / 文案都照后端抄：
 * - 最多 3 层 = `error_code::ERP_WAREHOUSE_LEVEL_EXCEEDED`(66004) 的口径；
 * - 库位只能挂叶子 = `ERP_WAREHOUSE_NOT_LEAF`(66005)；
 * - `is_leaf` 是派生值，`Create/UpdateWarehouseReq` 里**没有**这个字段（所以请求体里也不许出现）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  MAX_WAREHOUSE_LEVEL,
  allWarehouseIds,
  buildLocationCreatePayload,
  buildLocationUpdatePayload,
  buildWarehouseCreatePayload,
  buildWarehouseUpdatePayload,
  clampPage,
  deriveWarehouseLevel,
  emptyLocationForm,
  emptyWarehouseForm,
  filterWarehouseTree,
  findWarehouse,
  flattenWarehouses,
  initialExpandedIds,
  isEmptyUpdate,
  levelLabel,
  locationCreateGate,
  locationTypeLabel,
  paginateRows,
  parentPickableTree,
  leafWarehouseOptions,
  toggleExpandedId,
  treeDepth,
  validateLocationForm,
  validateWarehouseForm,
  warehouseFormFromModel,
  warehousePathText,
  warehouseTypeLabel,
  WAREHOUSE_TYPE_OPTIONS,
  LOCATION_TYPE_OPTIONS,
} from '../../src/views/business/erp/warehouse/warehouse-tree.ts';

/** 造一个仓库节点（只填断言用得到的字段） */
function w(
  id: string,
  name: string,
  patch: Partial<{
    code: string;
    level: number;
    isLeaf: boolean;
    parentId: string | null;
    type: string;
    children: any[];
  }> = {},
): any {
  return {
    id,
    warehouse_code: patch.code ?? name.toUpperCase(),
    warehouse_name: name,
    warehouse_type: patch.type ?? 'NORMAL',
    level: patch.level ?? 1,
    parent_id: patch.parentId ?? null,
    is_leaf: patch.isLeaf ?? (patch.children ? patch.children.length === 0 : true),
    warehouse_path: `/${id}/`,
    manager: null,
    allow_negative: false,
    address: null,
    status: 1,
    sort_order: 0,
    remark: null,
    created_at: '2026-10-30T00:00:00Z',
    updated_at: '2026-10-30T00:00:00Z',
    children: patch.children ?? [],
  };
}

/** 3 层样例树：总仓 → 华东分仓 → A区 →（A区下面还有库位，但库位不在树里） */
const A区 = w('a-qu', 'A区', { code: 'A', level: 3, isLeaf: true, parentId: 'hd' });
const B区 = w('b-qu', 'B区', { code: 'B', level: 3, isLeaf: true, parentId: 'hd' });
const 华东 = w('hd', '华东分仓', { code: 'HD', level: 2, isLeaf: false, parentId: 'zc', children: [A区, B区] });
const 华南 = w('hn', '华南分仓', { code: 'HN', level: 2, isLeaf: true, parentId: 'zc' });
const 总仓 = w('zc', '总仓', { code: 'ZC', level: 1, isLeaf: false, children: [华东, 华南] });
const 在途 = w('zt', '在途仓', { code: 'ZT', level: 1, isLeaf: true, type: 'IN_TRANSIT' });
const TREE = [总仓, 在途];

// ==================== ① 树结构 ====================

test('① 展平与层数：顶层只挂 parent 为空的仓库，深度算得出 3 层', () => {
  const flat = flattenWarehouses(TREE);
  // 展开顺序 = 父在前、子按后端给的顺序（sort_order 已在后端排好）
  assert.deepEqual(
    flat.map((row) => `${row.depth}:${row.node.id}`),
    ['1:zc', '2:hd', '3:a-qu', '3:b-qu', '2:hn', '1:zt'],
  );
  assert.equal(treeDepth(TREE), 3, '验收②要的「至少 3 层」就是这个数');
  assert.equal(allWarehouseIds(TREE).length, 6);
  // 顶层只挂 parent_id 为空的：总仓 / 在途仓
  assert.deepEqual(
    TREE.filter((node) => node.parent_id === null).map((node) => node.id),
    ['zc', 'zt'],
  );
});

test('② 首屏默认展开 = 第 1 层里有孩子的节点（不 expandAll）', () => {
  // 总仓有孩子 → 展开；在途仓是叶子 → 不进 expanded（否则树上一个空箭头）
  assert.deepEqual(initialExpandedIds(TREE), ['zc']);
  assert.deepEqual(initialExpandedIds([]), []);
});

test('③ 逐节点独立折叠：只动自己，不联级', () => {
  let expanded = ['zc', 'hd'];
  expanded = toggleExpandedId(expanded, 'hd'); // 收起华东
  assert.deepEqual(expanded, ['zc'], '收起华东不该顺手把总仓也收了');
  expanded = toggleExpandedId(expanded, 'hd'); // 再展开
  assert.deepEqual(expanded, ['zc', 'hd']);
  expanded = toggleExpandedId(expanded, 'nope'); // 不存在的 id → 追加
  assert.deepEqual(expanded, ['zc', 'hd', 'nope']);
  // 幂等：连按两次回到原状
  assert.deepEqual(toggleExpandedId(toggleExpandedId(['zc'], 'zc'), 'zc'), ['zc']);
});

test('④ 搜索：命中第 3 层时保留祖先并给出要展开的 id（否则子树静默消失）', () => {
  const hit = filterWarehouseTree(TREE, 'a区');
  assert.equal(hit.matchedCount, 1);
  // 祖先链 zc → hd 都还在（否则 A区 挂不上树，界面上就「搜不到」）
  assert.deepEqual(allWarehouseIds(hit.tree), ['zc', 'hd', 'a-qu']);
  assert.deepEqual(hit.expandedIds, ['zc', 'hd']);
  // 编码也能搜（忽略大小写）
  const byCode = filterWarehouseTree(TREE, 'hn');
  assert.deepEqual(allWarehouseIds(byCode.tree), ['zc', 'hn']);
  assert.equal(byCode.matchedCount, 1);
  // 空关键词 = 原树原样，不乱展开
  const all = filterWarehouseTree(TREE, '   ');
  assert.equal(all.tree.length, 2);
  assert.deepEqual(all.expandedIds, []);
  // 搜不到 → 空树 + 0 命中（页面显示空态）
  const none = filterWarehouseTree(TREE, '不存在的仓');
  assert.deepEqual(none.tree, []);
  assert.equal(none.matchedCount, 0);
});

test('⑤ 按 id 找节点 + 路径文字（右面板「当前选中」副标题）', () => {
  assert.equal(findWarehouse(TREE, 'a-qu')?.warehouse_name, 'A区');
  assert.equal(findWarehouse(TREE, 'zzz'), null);
  assert.equal(warehousePathText(TREE, 'a-qu'), '总仓 / 华东分仓 / A区');
  assert.equal(warehousePathText(TREE, 'zt'), '在途仓');
  assert.equal(warehousePathText(TREE, 'zzz'), '');
});

// ==================== ② 层级 / 叶子硬约束 ====================

test('⑥ 层级推导：顶级 = 1，第 3 层仓库不能再有下级（66004）', () => {
  assert.equal(deriveWarehouseLevel(null), 1);
  assert.equal(deriveWarehouseLevel(总仓), 2);
  assert.equal(deriveWarehouseLevel(华东), 3);
  assert.equal(deriveWarehouseLevel(A区), 4, '第 3 层再往下就超上限，校验里必须拦');
  assert.equal(MAX_WAREHOUSE_LEVEL, 3);

  const form = { ...emptyWarehouseForm(A区) } as any;
  form.warehouse_code = 'X01';
  form.warehouse_name = '不该存在的第 4 层';
  const errors = validateWarehouseForm(form, A区);
  assert.match(errors.parent_id, /最多 3 层/);
});

test('⑦ 叶子约束：非叶子仓库的「新建库位」必须禁用，且原因说清「不能设成叶子」', () => {
  const nonLeaf = locationCreateGate(华东);
  assert.equal(nonLeaf.disabled, true);
  assert.match(nonLeaf.reason, /还有下级仓库/);
  assert.match(nonLeaf.reason, /叶子仓库/);
  // 关键：`is_leaf` 是派生的，文案不能承诺「把它设为叶子」这种不存在的开关
  assert.doesNotMatch(nonLeaf.reason, /设为叶子/);

  assert.deepEqual(locationCreateGate(A区), { disabled: false, reason: '' });
  assert.equal(locationCreateGate(null).disabled, true);
  assert.match(locationCreateGate(null).reason, /先在左侧选中/);

  // 表单侧再拦一次（万一有人绕过按钮直接调接口）
  const form = { ...emptyLocationForm('hd') };
  form.location_code = 'A-01-01';
  const errors = validateLocationForm(form, 华东);
  assert.match(errors.warehouse_id, /还有下级仓库/);
  // 挂了叶子仓库就没事
  assert.deepEqual(validateLocationForm(form, A区), {});
});

// ==================== ③ 请求体 ====================

test('⑧ 新建仓库请求体：level 跟着父推导，**不带 is_leaf**，空串转 null', () => {
  const form = emptyWarehouseForm();
  form.warehouse_code = ' X01 ';
  form.warehouse_name = ' 测试分仓 ';
  form.warehouse_type = 'RAW';
  form.manager = '';
  form.address = ' 北京市 ';
  const payload = buildWarehouseCreatePayload(form, 总仓);
  assert.deepEqual(payload, {
    warehouse_code: 'X01',
    warehouse_name: '测试分仓',
    warehouse_type: 'RAW',
    parent_id: 'zc',
    level: 2,
    manager: null,
    address: '北京市',
    status: 1,
    remark: null,
  });
  assert.equal('is_leaf' in payload, false, 'CreateWarehouseReq 没有 is_leaf，发出去就是脏字段');
  // 顶级仓库：parent_id null + level 1（后端要求 (parent_id 为空) = (level = 1)）
  const top = buildWarehouseCreatePayload({ ...form, parent_id: null }, null);
  assert.equal(top.parent_id, null);
  assert.equal(top.level, 1);
});

test('⑨ 编辑仓库请求体：只带真改过的字段，清空的可空字段显式传 null', () => {
  const origin = { ...总仓, manager: '老王', address: '北京市', remark: null } as any;
  const form = warehouseFormFromModel(origin);
  assert.equal(isEmptyUpdate(buildWarehouseUpdatePayload(form, origin)), true, '没改就不该发 PUT');

  form.manager = ''; // 清空负责人
  form.warehouse_name = '总仓（改）';
  const payload = buildWarehouseUpdatePayload(form, origin);
  assert.deepEqual(payload, { warehouse_name: '总仓（改）', manager: null });
  assert.equal('warehouse_code' in payload, false, '编码是身份，UpdateWarehouseReq 没有这个字段');
  assert.equal('parent_id' in payload, false, '换父 = 换身份，后端编辑接口不接 parent_id');
});

test('⑩ 库位请求体：新建带 warehouse_id，编辑不带（后端不许换仓库）', () => {
  const form = emptyLocationForm('a-qu');
  form.location_code = ' A-01-01 ';
  form.location_name = '';
  form.location_type = 'PICKING';
  assert.deepEqual(buildLocationCreatePayload(form), {
    location_code: 'A-01-01',
    warehouse_id: 'a-qu',
    location_name: null,
    location_type: 'PICKING',
    status: 1,
  });

  const origin = {
    id: 'l1',
    location_code: 'A-01-01',
    location_name: 'A区一排一列',
    warehouse_id: 'a-qu',
    location_type: 'STORAGE',
    is_default: false,
    capacity: null,
    status: 1,
    sort_order: 0,
    remark: null,
    created_at: '',
    updated_at: '',
  } as any;
  const upd = buildLocationUpdatePayload({ ...emptyLocationForm('a-qu'), location_name: 'A区一排一列' }, origin);
  assert.deepEqual(upd, {}, '一个字段没改 → 空对象');
  const upd2 = buildLocationUpdatePayload(
    { ...emptyLocationForm('a-qu'), location_name: '', location_type: 'SCRAP' },
    origin,
  );
  assert.deepEqual(upd2, { location_name: null, location_type: 'SCRAP' });
  assert.equal('warehouse_id' in upd2, false);
});

// ==================== ④ 表单校验 / 枚举 / 分页 ====================

test('⑪ 仓库表单校验：编码 / 名称必填、长度上限照后端', () => {
  const form = emptyWarehouseForm();
  const errors = validateWarehouseForm(form, null);
  assert.equal(errors.warehouse_code, '请填仓库编码');
  assert.equal(errors.warehouse_name, '请填仓库名称');

  form.warehouse_code = 'X'.repeat(41);
  form.warehouse_name = '名'.repeat(121);
  form.manager = '人'.repeat(51);
  form.address = '址'.repeat(256);
  const long = validateWarehouseForm(form, null);
  assert.match(long.warehouse_code, /40/);
  assert.match(long.warehouse_name, /120/);
  assert.match(long.manager, /50/);
  assert.match(long.address, /255/);
});

test('⑫ 枚举文案与选项：仓库 5 类 / 库位 6 类，与后端常量逐字对齐', () => {
  assert.deepEqual(WAREHOUSE_TYPE_OPTIONS.map((item) => item.value), [
    'NORMAL',
    'RAW',
    'FINISHED',
    'SCRAP',
    'IN_TRANSIT',
  ]);
  assert.deepEqual(LOCATION_TYPE_OPTIONS.map((item) => item.value), [
    'STORAGE',
    'PICKING',
    'RECEIVING',
    'SHIPPING',
    'QUARANTINE',
    'SCRAP',
  ]);
  assert.equal(warehouseTypeLabel('IN_TRANSIT'), '在途仓');
  assert.equal(warehouseTypeLabel('WHATEVER'), 'WHATEVER', '未知值原样透出，不编中文');
  assert.equal(locationTypeLabel('PICKING'), '拣货位');
  assert.equal(levelLabel(1), '总仓');
  assert.equal(levelLabel(2), '分仓');
  assert.equal(levelLabel(3), '库房');
  assert.equal(levelLabel(4), '第 4 层');
});

test('⑬ 前端分页：切片 / 夹紧越界页码（第 3 页删光后不空白）', () => {
  const rows = Array.from({ length: 23 }, (_v, index) => index + 1);
  assert.deepEqual(paginateRows(rows, 1, 10), [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
  assert.deepEqual(paginateRows(rows, 3, 10), [21, 22, 23]);
  assert.deepEqual(paginateRows(rows, 9, 10), [21, 22, 23], '第 9 页 → 夹到最后一页');
  assert.deepEqual(paginateRows([], 2, 10), []);
  assert.equal(clampPage(9, 23, 10), 3);
  assert.equal(clampPage(0, 23, 10), 1);
  assert.equal(clampPage(1, 0, 10), 1);
});

test('⑭ 父仓库选择器：剪掉第 3 层（不能再往下建），父节点不因剪枝丢失', () => {
  const pruned = parentPickableTree(TREE);
  // 总仓（1）→ 华东（2）：第 3 层的 A区 / B区 被剪掉，但华东还在，不会整枝消失
  assert.deepEqual(allWarehouseIds(pruned), ['zc', 'hd', 'hn', 'zt']);
  assert.deepEqual(findWarehouse(pruned, 'hd')?.children, []);
  // 顶尖的叶子（在途仓）没有下级但 level 1，仍可作父
  assert.ok(findWarehouse(pruned, 'zt'));
});

test('⑮ 所属仓库下拉：只给叶子仓库，文案带完整路径 + 编码（平铺也分得清）', () => {
  const options = leafWarehouseOptions(TREE);
  assert.deepEqual(options.map((item) => item.value), ['a-qu', 'b-qu', 'hn', 'zt']);
  assert.deepEqual(options.map((item) => item.label), [
    '总仓 / 华东分仓 / A区（A）',
    '总仓 / 华东分仓 / B区（B）',
    '总仓 / 华南分仓（HN）',
    '在途仓（ZT）',
  ]);
  // 非叶子一个都不许出现（选了后端就 66005）
  assert.equal(options.some((item) => item.value === 'hd' || item.value === 'zc'), false);
});
