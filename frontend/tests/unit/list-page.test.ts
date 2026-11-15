/**
 * ListPage 纯逻辑单测（P3-04）。
 *
 * 运行：`pnpm test:unit`（= `tsx --test "tests/unit/**\/*.test.ts"`，零新依赖）。
 *
 * 为什么测 `list-page-pure.ts` 而不测 `.vue`：卡面要求「试点页功能与改造前一致」，
 * 而**行为差异全部藏在判定里** —— 空串筛选是整键缺席还是发空串（发空串后端查 0 行）、
 * 删到最后一页是停在空页还是回夹、导出 CSV 的引号/换行/BOM 对不对。
 * `.vue` 只负责画，这些漂了 `vue-tsc` 抓不到（编译期都是 string）。
 *
 * 断言口径：与 `views/business/erp/supplier/supplier-query.ts::buildSupplierQuery`
 * 对齐（空值整键缺席、trim、`0` 是真条件）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  activeFilterCount,
  buildListQuery,
  clampPage,
  clearFilter,
  csvFileName,
  emptyFilter,
  errorText,
  escapeCsvCell,
  exportColumns,
  isFiltered,
  isEmptyValue,
  paginationOf,
  rowsToCsv,
} from '@/components/ListPage/list-page-pure';
import type { ColumnDef, FilterDef } from '@/components/ListPage/types';

const FILTERS: FilterDef[] = [
  { key: 'keyword', type: 'keyword', placeholder: '搜编码 / 名称' },
  { key: 'status', type: 'select', options: [{ label: '启用', value: 1 }] },
  { key: 'min_qty', type: 'number' },
];

test('isEmptyValue：null / 空串 / 全空格 / 空数组算空；0 与 false 不算空', () => {
  assert.equal(isEmptyValue(null), true);
  assert.equal(isEmptyValue(undefined), true);
  assert.equal(isEmptyValue(''), true);
  assert.equal(isEmptyValue('   '), true);
  assert.equal(isEmptyValue([]), true);
  assert.equal(isEmptyValue([null, '']), true);
  assert.equal(isEmptyValue(0), false, '0 是真条件（状态 0 = 停用、金额 0 = 现结）');
  assert.equal(isEmptyValue(false), false);
  assert.equal(isEmptyValue('0'), false);
  assert.equal(isEmptyValue([0]), false);
});

test('emptyFilter：keyword / select 给空串，number 不设键（不塞 undefined 使控件受控）', () => {
  assert.deepEqual(emptyFilter(FILTERS), { keyword: '', status: '' });
  assert.deepEqual(emptyFilter(undefined), {});
  assert.deepEqual(emptyFilter([{ key: 'a', type: 'number', defaultValue: 5 }]), { a: 5 });
});

test('buildListQuery：空值整键缺席、字符串 trim、0/false 照发、page/page_size 必带', () => {
  const query = buildListQuery({ keyword: '  SUP  ', status: '', min_qty: 0 }, FILTERS, 2, 20);
  assert.deepEqual(query, { page: 2, page_size: 20, keyword: 'SUP', min_qty: 0 });
  assert.equal('status' in query, false, '`?status=` 会被后端当「等于空串」过滤，查出 0 行');
});

test('buildListQuery：没有 declarative 筛选时也带分页键', () => {
  assert.deepEqual(buildListQuery({}, undefined, 1, 50), { page: 1, page_size: 50 });
});

test('isFiltered / activeFilterCount：只数真条件', () => {
  assert.equal(isFiltered({ keyword: '', status: '' }, FILTERS), false);
  assert.equal(activeFilterCount({ keyword: ' x ', status: '' }, FILTERS), 1);
  assert.equal(isFiltered({ keyword: 'x', status: 0 }, FILTERS), true);
  assert.equal(activeFilterCount({ keyword: '', status: '', min_qty: 0 }, FILTERS), 1);
});

test('clampPage：删到最后一页最后一条时回夹，0/NaN 回 1，空列表回 1', () => {
  assert.equal(clampPage(3, 0, 20), 1, '空列表没有第 3 页');
  assert.equal(clampPage(5, 100, 20), 5);
  assert.equal(clampPage(6, 100, 20), 5);
  assert.equal(clampPage(1, 5, 20), 1);
  assert.equal(clampPage(0, 100, 20), 1);
  assert.equal(clampPage(Number.NaN, 100, 20), 1);
  assert.equal(clampPage(2.7, 100, 20), 2, '页码取整');
  assert.equal(clampPage(1, 100, 0), 1, 'pageSize 脏值不炸（按 1 条/页夹）');
});

test('paginationOf：分页器入参（服务端分页）', () => {
  const page = paginationOf(2, 20, 41);
  assert.equal(page.current, 2);
  assert.equal(page.pageSize, 20);
  assert.equal(page.total, 41);
  assert.equal(page.showJumper, true);
  assert.equal(page.showPageSize, false);
  assert.deepEqual(page.pageSizeOptions, [10, 20, 50, 100]);
  assert.equal(paginationOf(1, 10, 3, true).showPageSize, true);
});

test('errorText：Error / 带 message 的对象 / 字符串都吃', () => {
  assert.equal(errorText(new Error('后端说不行')), '后端说不行');
  assert.equal(errorText({ message: '码 66034：库存不足' }), '码 66034：库存不足');
  assert.equal(errorText({ message: '  ' }), '[object Object]', '空 message 不当文案');
  assert.equal(errorText('炸了'), '炸了');
});

test('escapeCsvCell：逗号 / 引号 / 换行加引号，内部引号翻倍；数组、对象、空值各有口径', () => {
  assert.equal(escapeCsvCell('普通'), '普通');
  assert.equal(escapeCsvCell('a,b'), '"a,b"');
  assert.equal(escapeCsvCell('他说"好"'), '"他说""好"""');
  assert.equal(escapeCsvCell('第一行\n第二行'), '"第一行\n第二行"');
  assert.equal(escapeCsvCell(null), '');
  assert.equal(escapeCsvCell(undefined), '');
  assert.equal(escapeCsvCell(0), '0');
  assert.equal(escapeCsvCell(['a', 'b']), 'a;b');
  assert.equal(escapeCsvCell({ a: 1 }), '"{""a"":1}"');
});

test('exportColumns：op / row-select / 无 colKey / exportable:false 都不进导出', () => {
  const columns: ColumnDef[] = [
    { colKey: 'supplier_code', title: '供应商编码' },
    { colKey: 'op', title: '操作' },
    { colKey: 'row-select', type: 'multiple' },
    { colKey: 'attach', title: '附件', exportable: false },
    { colKey: 'status', title: '状态' },
  ];
  assert.deepEqual(
    exportColumns(columns).map((column) => column.colKey),
    ['supplier_code', 'status'],
  );
});

test('rowsToCsv：BOM + 表头取 title + CRLF，值里逗号不串列', () => {
  const columns: ColumnDef[] = [
    { colKey: 'supplier_code', title: '供应商编码' },
    { colKey: 'supplier_name', title: '名称' },
    { colKey: 'op', title: '操作' },
  ];
  const csv = rowsToCsv(
    [
      { supplier_code: 'SUP-0001', supplier_name: '示例供应商' },
      { supplier_code: 'SUP-0002', supplier_name: '甲,乙公司' },
    ],
    columns,
  );
  assert.equal(csv.startsWith('\ufeff'), true, '缺 BOM 时 Excel 打开是乱码');
  const lines = csv.replace(/^\ufeff/, '').trimEnd().split('\r\n');
  assert.deepEqual(lines, ['供应商编码,名称', 'SUP-0001,示例供应商', 'SUP-0002,"甲,乙公司"']);
});

test('rowsToCsv：colKey 支持 a.b 嵌套，缺字段给空串（不出 undefined）', () => {
  const csv = rowsToCsv(
    [{ a: { b: '内层' } }, {}],
    [
      { colKey: 'a.b', title: '嵌套' },
      { colKey: 'missing', title: '缺列' },
    ],
  );
  const lines = csv.replace(/^\ufeff/, '').trimEnd().split('\r\n');
  assert.deepEqual(lines, ['嵌套,缺列', '内层,', ',']);
});

test('csvFileName：本地时间戳到分钟，非法字符剔除', () => {
  const name = csvFileName('供应商', new Date(2026, 10, 15, 17, 30, 45));
  assert.equal(name, '供应商-20261115-1730.csv');
  assert.equal(csvFileName('a/b:c*?', new Date(2026, 0, 2, 3, 4)), 'abc-20260102-0304.csv');
  assert.equal(csvFileName('', new Date(2026, 0, 2, 3, 4)), '列表-20260102-0304.csv');
});

test('clearFilter：keyword / select 回空串，custom（日期区间、复选框）回 undefined', () => {
  const defs = [
    { key: 'keyword', type: 'keyword' as const },
    { key: 'status', type: 'select' as const },
    { key: 'date_range', type: 'custom' as const },
    { key: 'convertible_only', type: 'custom' as const },
  ];
  const filter: Record<string, unknown> = {
    keyword: 'AP-001',
    status: 'DRAFT',
    date_range: ['2026-11-01', '2026-11-30'],
    convertible_only: true,
  };
  clearFilter(defs, filter);
  assert.deepEqual(filter, { keyword: '', status: '', date_range: undefined, convertible_only: undefined });
});

test('clearFilter：defaultValue 回默认值（不是清成空）', () => {
  const filter: Record<string, unknown> = { page_size_hint: 50 };
  clearFilter([{ key: 'page_size_hint', type: 'number' as const, defaultValue: 20 }], filter);
  assert.equal(filter.page_size_hint, 20);
});

test('clearFilter：声明外的键不动（页面私有状态不该被列表组件清掉）', () => {
  const filter: Record<string, unknown> = { keyword: 'x', internalTab: 'mine' };
  clearFilter([{ key: 'keyword', type: 'keyword' as const }], filter);
  assert.equal(filter.keyword, '');
  assert.equal(filter.internalTab, 'mine');
});

test('clearFilter 之后 isFiltered 必须是 false（否则空态文案会一直说「没有匹配」）', () => {
  const defs = [
    { key: 'status', type: 'select' as const },
    { key: 'date_range', type: 'custom' as const },
  ];
  const filter: Record<string, unknown> = { status: 'POSTED', date_range: ['2026-11-01', '2026-11-02'] };
  assert.equal(isFiltered(filter, defs), true);
  clearFilter(defs, filter);
  assert.equal(isFiltered(filter, defs), false);
});
