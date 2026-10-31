/**
 * 库位管理页（P4-B）纯逻辑单测：仓库筛选 / 类型映射 / 启用状态映射 / 行视图翻译。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖、无 DOM）。
 *
 * 为什么测 `.ts` 不测 `.vue`：本页最关键的三件事 —— **请求参数带不带 warehouse_id**、
 * **后端枚举翻成什么中文**、**停用（status=0）会不会被当成「不限」** —— 全在
 * `location-logic.ts`；`.vue` 只渲染。这几处一旦漂移，`vue-tsc` 抓不到，
 * 界面上也看不出来（筛选「什么都没发生」和「筛出全部」长得一样）。
 *
 * 断言口径照后端抄（不是照卡面草图）：
 * - 字段名 / 枚举 = `backend/src/business/erp/dto.rs`（`LocationRes` / `LOCATION_TYPES` / `LocationFilter`）；
 * - `status` 是 `smallint` 1/0（`V0137__erp_master_data_and_warehouse.up.sql` 的 `biz_location.status`）；
 * - 分页来自 `GET /business/erp/location/page`（服务端分页，`model.rs::location_page` 只认
 *   `keyword / warehouse_id / location_type / status / page / page_size`）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import type { Location, WarehouseTreeNode } from '@/api/business/erp';
import {
  LOCATION_PAGE_SIZE,
  LOCATION_STATUS_OPTIONS,
  LOCATION_TYPE_OPTIONS,
  buildLocationQuery,
  capacityText,
  emptyLocationFilter,
  formatTime,
  isLocationFiltered,
  locationStatusLabel,
  locationStatusTheme,
  locationTypeLabel,
  locationTypeTheme,
  toLocationRow,
  warehouseFilterOptions,
  warehouseNameOf,
} from '@/views/business/erp/location/location-logic';

/** 种子仓库树（形状抄 `GET /api/business/erp/warehouse/tree` 的真出参：总仓 → 分仓 → 库房） */
const TREE: WarehouseTreeNode[] = [
  {
    id: 'wh-root',
    warehouse_code: 'WH01',
    warehouse_name: '总仓',
    warehouse_type: 'NORMAL',
    manager: null,
    allow_negative: false,
    address: null,
    status: 1,
    sort_order: 1,
    remark: null,
    parent_id: null,
    level: 1,
    warehouse_path: '/wh-root/',
    is_leaf: false,
    created_at: '2026-10-30T08:52:10.282882Z',
    updated_at: '2026-10-30T08:52:10.282882Z',
    children: [
      {
        id: 'wh-branch',
        warehouse_code: 'WH02',
        warehouse_name: '分仓',
        warehouse_type: 'NORMAL',
        manager: null,
        allow_negative: false,
        address: null,
        status: 1,
        sort_order: 10,
        remark: null,
        parent_id: 'wh-root',
        level: 2,
        warehouse_path: '/wh-root/wh-branch/',
        is_leaf: false,
        created_at: '2026-10-30T08:52:10.282882Z',
        updated_at: '2026-10-30T08:52:10.282882Z',
        children: [
          {
            id: 'wh-leaf',
            warehouse_code: 'WH02-A',
            warehouse_name: 'A区',
            warehouse_type: 'NORMAL',
            manager: null,
            allow_negative: false,
            address: null,
            status: 1,
            sort_order: 10,
            remark: null,
            parent_id: 'wh-branch',
            level: 3,
            warehouse_path: '/wh-root/wh-branch/wh-leaf/',
            is_leaf: true,
            created_at: '2026-10-30T08:52:10.282882Z',
            updated_at: '2026-10-30T08:52:10.282882Z',
            children: [],
          },
        ],
      },
    ],
  },
  {
    id: 'wh-transit',
    warehouse_code: 'WH90',
    warehouse_name: '在途仓',
    warehouse_type: 'IN_TRANSIT',
    manager: null,
    allow_negative: false,
    address: null,
    status: 1,
    sort_order: 90,
    remark: null,
    parent_id: null,
    level: 1,
    warehouse_path: '/wh-transit/',
    is_leaf: true,
    created_at: '2026-10-30T08:52:10.282882Z',
    updated_at: '2026-10-30T08:52:10.282882Z',
    children: [],
  },
];

/** 一行库位（字段逐字抄 `LocationRes`） */
function locationRow(overrides: Partial<Location> = {}): Location {
  return {
    id: 'loc-1',
    location_code: 'A-01-01',
    location_name: 'A区一排一列',
    warehouse_id: 'wh-leaf',
    location_type: 'STORAGE',
    is_default: true,
    capacity: null,
    status: 1,
    sort_order: 1,
    remark: null,
    created_at: '2026-10-30T08:52:10.282882Z',
    updated_at: '2026-10-30T09:10:00.000000Z',
    ...overrides,
  };
}

// ==================== ① 仓库筛选 → 请求参数 ====================

test('① 仓库筛选：只选仓库时，请求参数带 warehouse_id 且不带其它筛选键', () => {
  const query = buildLocationQuery({ ...emptyLocationFilter(), warehouse_id: 'wh-leaf' }, 1);
  assert.equal(query.warehouse_id, 'wh-leaf');
  assert.equal(query.page, 1);
  assert.equal(query.page_size, LOCATION_PAGE_SIZE);
  // 没填的筛选**整键缺席**（发空串后端会当「等于空串」过滤）
  assert.equal('keyword' in query, false);
  assert.equal('location_type' in query, false);
  assert.equal('status' in query, false);
});

test('① 仓库筛选：不选仓库 = 跨仓总览（warehouse_id 整键缺席，不是空串）', () => {
  const query = buildLocationQuery(emptyLocationFilter(), 2);
  assert.equal('warehouse_id' in query, false);
  assert.equal(query.page, 2);
});

test('① 仓库筛选：换仓库 → 参数跟着换；关键词去掉首尾空格', () => {
  const first = buildLocationQuery({ ...emptyLocationFilter(), warehouse_id: 'wh-branch' }, 1);
  const second = buildLocationQuery({ ...emptyLocationFilter(), warehouse_id: 'wh-leaf' }, 1);
  assert.equal(first.warehouse_id, 'wh-branch');
  assert.equal(second.warehouse_id, 'wh-leaf');
  assert.notEqual(first.warehouse_id, second.warehouse_id);

  const spaced = buildLocationQuery({ ...emptyLocationFilter(), keyword: '  A-01  ' }, 1);
  assert.equal(spaced.keyword, 'A-01');
});

// ==================== ② 库位类型映射 ====================

test('② 库位类型映射：后端 6 个枚举 → 中文（逐字对齐 dto.rs::LOCATION_TYPES）', () => {
  assert.deepEqual(
    LOCATION_TYPE_OPTIONS.map((item) => item.value),
    ['STORAGE', 'PICKING', 'RECEIVING', 'SHIPPING', 'QUARANTINE', 'SCRAP'],
  );
  assert.equal(locationTypeLabel('STORAGE'), '存储位');
  assert.equal(locationTypeLabel('PICKING'), '拣货位');
  assert.equal(locationTypeLabel('RECEIVING'), '收货位');
  assert.equal(locationTypeLabel('SHIPPING'), '发货位');
  assert.equal(locationTypeLabel('QUARANTINE'), '待检位');
  assert.equal(locationTypeLabel('SCRAP'), '报废位');
  // 未知值原样回显（不静默变空白）
  assert.equal(locationTypeLabel('UNKNOWN'), 'UNKNOWN');
});

test('② 库位类型筛选：选中的类型进请求参数（原样大写枚举，不传中文）', () => {
  const query = buildLocationQuery({ ...emptyLocationFilter(), location_type: 'PICKING' }, 1);
  assert.equal(query.location_type, 'PICKING');
});

test('② 库位类型配色：每种类型都有 TDesign 主题，不写裸 hex', () => {
  const themes = ['STORAGE', 'PICKING', 'RECEIVING', 'SHIPPING', 'QUARANTINE', 'SCRAP'].map(
    (type) => locationTypeTheme(type),
  );
  for (const theme of themes) {
    assert.ok(
      ['primary', 'success', 'warning', 'danger', 'default'].includes(theme),
      `${theme} 应是 TDesign 主题名`,
    );
  }
  assert.equal(locationTypeTheme('STORAGE'), 'default');
  assert.equal(locationTypeTheme('SCRAP'), 'danger');
});

// ==================== ③ 启用状态映射 ====================

test('③ 启用状态映射：1 启用（success）/ 0 停用（default），未知值可见', () => {
  assert.equal(locationStatusLabel(1), '启用');
  assert.equal(locationStatusLabel(0), '停用');
  assert.equal(locationStatusTheme(1), 'success');
  assert.equal(locationStatusTheme(0), 'default');
  // 脏数据不静默成空白
  assert.equal(locationStatusLabel(9), '未知（9）');
});

test('③ 启用状态筛选：选「停用」必须发出 status=0（number），不能被真假值判断丢掉', () => {
  const disabled = buildLocationQuery({ ...emptyLocationFilter(), status: '0' }, 1);
  assert.equal(disabled.status, 0);
  assert.equal(typeof disabled.status, 'number');
  // 而「不限」时整键缺席（不是 0、也不是 '')
  const all = buildLocationQuery(emptyLocationFilter(), 1);
  assert.equal('status' in all, false);
  // 下拉里「停用」是真选项
  assert.deepEqual(LOCATION_STATUS_OPTIONS.map((item) => item.value), ['1', '0']);
});

// ==================== ④ 仓库维度 / 行视图 ====================

test('④ 所属仓库：列表接口没有 warehouse_name，用仓库树翻译成层级路径', () => {
  assert.equal(warehouseNameOf(TREE, 'wh-leaf'), '总仓 / 分仓 / A区');
  assert.equal(warehouseNameOf(TREE, 'wh-transit'), '在途仓');
  // 树里查不到 → uuid 前 8 位（不留空白）
  assert.equal(warehouseNameOf(TREE, '0f0f0f0f-1111-2222-3333-444455556666'), '0f0f0f0f');
  assert.equal(warehouseNameOf(TREE, ''), '—');
  assert.equal(warehouseNameOf(TREE, null), '—');
});

test('④ 仓库筛选下拉：列出**全部**仓库（非叶子也在），文案带层级路径与编码', () => {
  const options = warehouseFilterOptions(TREE);
  assert.deepEqual(
    options.map((item) => item.value),
    ['wh-root', 'wh-branch', 'wh-leaf', 'wh-transit'],
  );
  const leaf = options.find((item) => item.value === 'wh-leaf');
  assert.equal(leaf?.label, '总仓 / 分仓 / A区（WH02-A）');
  assert.equal(leaf?.is_leaf, true);
  // 非叶子也进筛选（否则挂非叶子仓库的历史库位永远筛不出来）
  const root = options.find((item) => item.value === 'wh-root');
  assert.ok(root, '非叶子仓库必须出现在筛选下拉里');
  assert.equal(root?.is_leaf, false);
});

test('④ 行视图：类型/状态/仓库名/容量全部翻译好，原字段不动', () => {
  const view = toLocationRow(locationRow({ location_type: 'PICKING', status: 0 }), TREE);
  assert.equal(view.type_label, '拣货位');
  assert.equal(view.status_label, '停用');
  assert.equal(view.warehouse_text, '总仓 / 分仓 / A区');
  assert.equal(view.capacity_text, '不限');
  // 原字段原样保留（页面里仍然用 location_code / is_default 渲染）
  assert.equal(view.location_code, 'A-01-01');
  assert.equal(view.warehouse_id, 'wh-leaf');
  assert.equal(view.is_default, true);

  assert.equal(capacityText('120.0000'), '120.0000');
  assert.equal(capacityText(0), '0');
  assert.equal(capacityText(null), '不限');
});

test('⑤ 是否处于筛选态：只看停用也算筛选（否则空态会说「还没有库位」）', () => {
  assert.equal(isLocationFiltered(emptyLocationFilter()), false);
  assert.equal(isLocationFiltered({ ...emptyLocationFilter(), warehouse_id: 'wh-leaf' }), true);
  assert.equal(isLocationFiltered({ ...emptyLocationFilter(), status: '0' }), true);
  assert.equal(isLocationFiltered({ ...emptyLocationFilter(), keyword: 'A-01' }), true);
  // 纯空格不算筛选
  assert.equal(isLocationFiltered({ ...emptyLocationFilter(), keyword: '   ' }), false);
});

test('⑥ 时间格式化：RFC3339 → YYYY-MM-DD HH:mm，非法/空值回「—」', () => {
  const text = formatTime('2026-10-30T09:10:00.000000Z');
  assert.match(text, /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}$/);
  assert.equal(formatTime(null), '—');
  assert.equal(formatTime('not-a-date'), '—');
});
