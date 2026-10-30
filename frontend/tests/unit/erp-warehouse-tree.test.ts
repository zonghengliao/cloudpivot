/**
 * W4a-1 单测（4 条）：仓库树的**扁平化 / 节点 key / 叶子判定 / 选中**。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * 为什么测 `warehouse-tree.ts` 不测 `.vue`：这四条全是「数据进、结果出」的纯判断，
 * `.vue` 只把它们绑到 `t-tree` 上。放在纯逻辑层，`node --test` 能直接跑，
 * 也不用为一次断言装 jsdom。
 *
 * 断言里的字段名照后端抄（`backend/src/business/erp/dto.rs` `WarehouseRes`）：
 * `warehouse_code` / `warehouse_name` / `warehouse_path` / `parent_id` / `level` / `is_leaf`，
 * **不是** `code` / `name` / `path` —— 写错字段名的测试等于测了个假对象。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  flattenWarehouses,
  resolveIsLeaf,
  selectWarehouseId,
  treeDepth,
  warehouseNodeKey,
  warehouseTreeKeys,
} from '../../src/views/business/erp/warehouse/warehouse-tree.ts';

/** 造一个仓库节点（只填本文件断言用得到的字段，其余照 `WarehouseRes` 补齐） */
function w(
  id: string,
  name: string,
  patch: Partial<{
    code: string;
    level: number;
    isLeaf: boolean;
    parentId: string | null;
    path: string;
    type: string;
    children: any[];
    status: number;
  }> = {},
): any {
  return {
    id,
    warehouse_code: patch.code ?? name.toUpperCase(),
    warehouse_name: name,
    warehouse_type: patch.type ?? 'NORMAL',
    level: patch.level ?? 1,
    parent_id: patch.parentId ?? null,
    is_leaf: patch.isLeaf ?? true,
    warehouse_path: patch.path ?? `/${id}/`,
    manager: null,
    allow_negative: false,
    address: null,
    status: patch.status ?? 1,
    sort_order: 0,
    remark: null,
    created_at: '2026-10-30T00:00:00Z',
    updated_at: '2026-10-30T00:00:00Z',
    children: patch.children ?? [],
  };
}

/** 3 层样例树：总仓(1) → 华东分仓(2) → A区/B区(3)，另有第二棵总仓树 + 一个在途仓 */
const A区 = w('a-qu', 'A区', { code: 'A', level: 3, isLeaf: true, parentId: 'hd', path: '/zc/hd/a-qu/' });
const B区 = w('b-qu', 'B区', { code: 'B', level: 3, isLeaf: true, parentId: 'hd', path: '/zc/hd/b-qu/' });
const 华东 = w('hd', '华东分仓', {
  code: 'HD',
  level: 2,
  isLeaf: false,
  parentId: 'zc',
  path: '/zc/hd/',
  children: [A区, B区],
});
const 华南 = w('hn', '华南分仓', {
  code: 'HN',
  level: 2,
  isLeaf: false,
  parentId: 'zc',
  path: '/zc/hn/',
  children: [w('hn-1', 'C区', { code: 'C', level: 3, isLeaf: true, parentId: 'hn', path: '/zc/hn/hn-1/' })],
});
const 总仓 = w('zc', '总仓', {
  code: 'ZC',
  level: 1,
  isLeaf: false,
  path: '/zc/',
  children: [华东, 华南],
});
const 在途 = w('zt', '在途仓', { code: 'ZT', level: 1, isLeaf: true, type: 'IN_TRANSIT' });
const TREE = [总仓, 在途];

// ==================== ① 扁平化：长度 + parent 关系 ====================

test('① 3 层树展平：长度 = 节点总数，parent_id 仍指向上级真实节点', () => {
  const flat = flattenWarehouses(TREE);

  // 7 个节点：总仓、华东、A区、B区、华南、C区、在途仓
  assert.equal(flat.length, 7, '展平不能多也不能少');
  assert.equal(treeDepth(TREE), 3, '第 3 层是「库房」，不能再有第 4 层');

  // 展平顺序 = 父在前（深度优先），深度依次是 1/2/3
  assert.deepEqual(
    flat.map((row) => `${row.depth}:${row.node.id}`),
    ['1:zc', '2:hd', '3:a-qu', '3:b-qu', '2:hn', '3:hn-1', '1:zt'],
  );

  // parent 关系保留：每个非根节点的 parent_id 都能在 id 索引里指回它的上级，
  // 且上级深度 = 自己深度 - 1（物化路径树「层级由上级推导」的直接体现）
  const byId = new Map(flat.map((row) => [row.node.id, row]));
  for (const row of flat) {
    if (row.node.parent_id === null) {
      assert.equal(row.depth, 1, `根节点 ${row.node.id} 的深度必须是 1`);
      continue;
    }
    const parent = byId.get(row.node.parent_id);
    assert.ok(parent, `${row.node.id} 的 parent_id=${row.node.parent_id} 必须在树里`);
    assert.equal(parent!.depth, row.depth - 1, `${row.node.id} 的深度必须比上级大 1`);
  }

  // 叶子节点不丢：3 个第 3 层 + 在途仓
  assert.equal(flat.filter((row) => row.node.level === 3).length, 3);
});

// ==================== ② 节点 key：空 code / 重复 code ====================

test('② 节点 key 生成：空 code 不产出 undefined，重复 code 也不撞 key', () => {
  // 正常路径：id 优先
  assert.equal(warehouseNodeKey(w('zc', '总仓', { code: 'ZC' })), 'wh:zc');

  // 空 id + 空 code：退到序号，绝不返回 undefined / 空串
  const empty = warehouseNodeKey({ id: '', warehouse_code: '', warehouse_path: '' }, 3);
  assert.equal(empty, 'wh:#3');
  assert.ok(empty.length > 0);
  assert.ok(!empty.includes('undefined'));

  // 空 id + 有 code：退到 code
  assert.equal(warehouseNodeKey({ id: '', warehouse_code: 'HD', warehouse_path: '' }), 'code:HD');

  // 空 id + 既无 code 也无 path（脏数据 / 新增未落库）：key 非空且互不相同
  const keys = warehouseTreeKeys([
    { ...w('', '空1', { code: '', path: '' }) },
    { ...w('', '空2', { code: '', path: '' }) },
  ]);
  assert.equal(keys.length, 2);
  assert.equal(new Set(keys).size, 2, '两个空节点必须拿到两个不同的 key');
  assert.ok(keys.every((key) => key.length > 0 && !key.includes('undefined')));

  // 重复 code（同一编码挂两次，数据脏）：第一个保持原 key，后来者补序号
  const dup = warehouseTreeKeys([
    { ...w('', '重复1', { code: 'DUP', path: '' }) },
    { ...w('', '重复2', { code: 'DUP', path: '' }) },
  ]);
  assert.deepEqual(dup, ['code:DUP', 'code:DUP~1']);

  // 整棵 3 层树的 key：数量对得上，且没有重复
  const treeKeys = warehouseTreeKeys(TREE);
  assert.equal(treeKeys.length, flattenWarehouses(TREE).length);
  assert.equal(new Set(treeKeys).size, treeKeys.length, '整棵树不许有重复 key');
});

// ==================== ③ is_leaf：children 优先 ====================

test('③ is_leaf 判定：有 children 的为 false，无 children 的为 true', () => {
  // 有 children → 非叶子（哪怕后端 is_leaf 漏更新成 true，也以 children 为准）
  assert.equal(resolveIsLeaf(华东), false);
  assert.equal(resolveIsLeaf({ ...华东, is_leaf: true }), false, 'children 优先于 is_leaf');

  // 无 children（空数组 / undefined）→ 叶子（照后端 is_leaf）
  assert.equal(resolveIsLeaf(A区), true);
  assert.equal(resolveIsLeaf({ ...A区, children: [] }), true);
  assert.equal(resolveIsLeaf({ ...A区, children: undefined }), true);

  // 列表接口的行没有 children 字段：只看 is_leaf，不二次猜
  assert.equal(resolveIsLeaf({ is_leaf: true }), true);
  assert.equal(resolveIsLeaf({ is_leaf: false }), false);
});

// ==================== ④ 选中：selectedWarehouseId 更新 ====================

test('④ 点节点后 selectedWarehouseId 正确更新（含取消选中）', () => {
  // 页面里的状态就这一个字符串，模拟 `selectedId = ref(null)`
  let selectedWarehouseId: string | null = null;
  assert.equal(selectedWarehouseId, null, '初始未选中');

  // 点第 3 层节点 → 记它的 id（不是 code、不是 path）
  selectedWarehouseId = selectWarehouseId(A区);
  assert.equal(selectedWarehouseId, 'a-qu');

  // 再点另一个节点 → 覆盖
  selectedWarehouseId = selectWarehouseId(华东);
  assert.equal(selectedWarehouseId, 'hd');

  // 点已选中的同一个节点：保持（t-tree 的 click 可能重复触发，不许变成 null）
  selectedWarehouseId = selectWarehouseId(华东);
  assert.equal(selectedWarehouseId, 'hd');

  // 空 id / 脏空格 → 视为取消选中（右侧回空态），不抛错、不写空串进去
  assert.equal(selectWarehouseId({ id: '' }), null);
  assert.equal(selectWarehouseId({ id: '   ' }), null);
  assert.equal(selectWarehouseId(null), null);
  assert.equal(selectWarehouseId(undefined), null);

  // 刷新后节点消失（整棵树重来）也要能兜住
  selectedWarehouseId = selectWarehouseId(null);
  assert.equal(selectedWarehouseId, null);
});
