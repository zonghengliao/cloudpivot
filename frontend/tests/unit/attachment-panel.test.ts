/**
 * ATT-03 单测：附件面板的纯逻辑（类型图标 / 大小格式化 / 重排）+ 组件与接口的关键约束。
 *
 * # 钉死的口径（卡面验收 ①④ + 图标不空白的既有坑）
 *
 * 1. **大小格式化**：`< 1KB → B`、`< 1MB → KB`、否则 `MB`；`1536000` 必须显示 `1.5 MB`
 *    而不是 `1536000 B`（验收④）。
 * 2. **类型图标按扩展名**：pdf / word / excel / ppt / image / zip / other 七档，
 *    大写扩展名也要认；扩展名缺失才用 MIME 兜底。
 * 3. **图标名必须真实存在**：写错的 TDesign 图标名不报错、只渲染成空白
 *    （AGENTS.md 高频踩坑第二条）→ 逐个回查 `node_modules` 里的组件文件。
 * 4. **面板与接口契约**：面板必须用 `formatAttachmentSize`（不许自己再写一份）、
 *    必须给上移 / 下移与图标留 `data-testid`；接口必须带 `sort` 参数与
 *    `sort_order` 字段（否则手动排序落不了库）。
 *
 * 运行：`pnpm test:unit`
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  KIND_EXTENSIONS,
  KIND_ICONS,
  KIND_THEMES,
  describeAttachment,
  fileKindLabel,
  fileKindOf,
  formatAttachmentSize,
  moveWithin,
} from '../../src/components/attachment-panel-pure';

const HERE = dirname(fileURLToPath(import.meta.url));
const SRC = resolve(HERE, '../../src');
const ROOT = resolve(HERE, '../..');

function readSource(relative: string): string {
  return readFileSync(join(SRC, relative), 'utf8');
}

// ==================== ① 大小格式化（验收④） ====================

test('大小格式化：B / KB / MB 三段，1 位小数且不留 .0', () => {
  // < 1 KB → 整数 B
  assert.equal(formatAttachmentSize(0), '0 B');
  assert.equal(formatAttachmentSize(512), '512 B');
  assert.equal(formatAttachmentSize(1023), '1023 B');

  // 1 KB ≤ size < 1 MB → KB
  assert.equal(formatAttachmentSize(1024), '1 KB');
  assert.equal(formatAttachmentSize(1536), '1.5 KB');
  assert.equal(formatAttachmentSize(2048), '2 KB');
  assert.equal(formatAttachmentSize(1024 * 1024 - 1), '1 MB', '差 1 字节也要进位到 MB（不能写成 1024 KB）');

  // ≥ 1 MB → MB
  assert.equal(formatAttachmentSize(1024 * 1024), '1 MB');
  assert.equal(formatAttachmentSize(1024 * 1024 * 2.25), '2.3 MB');
  assert.equal(formatAttachmentSize(100 * 1024 * 1024), '100 MB');
});

test('大小格式化：验收④ 的原值 1536000 字节 = 1.5 MB（不是 1536000 B）', () => {
  assert.equal(formatAttachmentSize(1_536_000), '1.5 MB');
});

test('大小格式化：空值 / 负数 / NaN 一律 "-"（不显示 NaN B / -1 B）', () => {
  assert.equal(formatAttachmentSize(null), '-');
  assert.equal(formatAttachmentSize(undefined), '-');
  assert.equal(formatAttachmentSize(-1), '-');
  assert.equal(formatAttachmentSize(Number.NaN), '-');
  assert.equal(formatAttachmentSize(Number.POSITIVE_INFINITY), '-');
});

// ==================== ② 类型图标（验收①） ====================

test('类型判定：pdf / word / excel / ppt / image / zip / other 七档', () => {
  assert.equal(fileKindOf('合同扫描件.pdf'), 'pdf');
  assert.equal(fileKindOf('合同扫描件.PDF'), 'pdf', '大写扩展名也要认');

  for (const ext of ['doc', 'docx', 'wps']) {
    assert.equal(fileKindOf(`说明书.${ext}`), 'word', `${ext} 应判为 word`);
  }
  for (const ext of ['xls', 'xlsx', 'csv', 'et']) {
    assert.equal(fileKindOf(`对账.${ext}`), 'excel', `${ext} 应判为 excel`);
  }
  for (const ext of ['ppt', 'pptx', 'dps']) {
    assert.equal(fileKindOf(`汇报.${ext}`), 'ppt', `${ext} 应判为 ppt`);
  }
  for (const ext of ['jpg', 'jpeg', 'png', 'webp', 'gif', 'bmp', 'svg']) {
    assert.equal(fileKindOf(`照片.${ext.toUpperCase()}`), 'image', `${ext} 应判为 image`);
  }
  for (const ext of ['zip', 'rar', '7z', 'gz', 'tar', 'bz2', 'xz']) {
    assert.equal(fileKindOf(`归档.${ext}`), 'zip', `${ext} 应判为 zip`);
  }

  // other：认识的扩展名之外 / 没有扩展名 / MIME 也不认识
  assert.equal(fileKindOf('说明.txt'), 'other');
  assert.equal(fileKindOf('发票.ofd'), 'other');
  assert.equal(fileKindOf('没有扩展名'), 'other');
  assert.equal(fileKindOf(''), 'other');
});

test('类型判定：扩展名缺失或陌生时用 MIME 兜底', () => {
  assert.equal(fileKindOf('无扩展名', 'application/pdf'), 'pdf');
  assert.equal(fileKindOf('blob', 'image/png'), 'image');
  assert.equal(fileKindOf(
    '统计',
    'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet',
  ), 'excel');
  assert.equal(fileKindOf(
    '汇报',
    'application/vnd.openxmlformats-officedocument.presentationml.presentation',
  ), 'ppt');
  assert.equal(fileKindOf(
    '说明',
    'application/vnd.openxmlformats-officedocument.wordprocessingml.document',
  ), 'word');
  assert.equal(fileKindOf('打包', 'application/zip'), 'zip');
  assert.equal(fileKindOf('未知', 'application/octet-stream'), 'other');
  // 扩展名优先于 MIME（后端按扩展名存，前端也按扩展名判，两边不漂移）
  assert.equal(fileKindOf('对账.xlsx', 'image/png'), 'excel');
});

test('图标名 / 配色 / 文案：七档全覆盖且文案不重复', () => {
  const kinds = Object.keys(KIND_ICONS) as (keyof typeof KIND_ICONS)[];
  assert.deepEqual(
    [...kinds].sort(),
    ['excel', 'image', 'other', 'pdf', 'ppt', 'word', 'zip'],
    '七档必须齐全（卡面：pdf/word/excel/ppt/image/zip/other）',
  );
  const labels = new Set<string>();
  for (const kind of kinds) {
    assert.ok(KIND_ICONS[kind], `${kind} 缺图标名`);
    assert.ok(KIND_THEMES[kind], `${kind} 缺配色`);
    assert.ok(fileKindLabel(kind), `${kind} 缺文案`);
    assert.ok(KIND_EXTENSIONS[kind], `${kind} 缺扩展名清单`);
    labels.add(fileKindLabel(kind));
  }
  assert.equal(labels.size, kinds.length, '七档文案必须互不相同（否则界面上分不清）');
});

test('图标名在 tdesign-icons-vue-next 里真实存在（写错只会渲染空白）', () => {
  for (const [kind, icon] of Object.entries(KIND_ICONS)) {
    const file = join(ROOT, 'node_modules/tdesign-icons-vue-next/esm/components', `${icon}.js`);
    assert.ok(existsSync(file), `${kind} 的图标 ${icon} 不存在（${file}）→ 界面会空白`);
  }
});

test('describeAttachment：一次拿到图标 / 配色 / 类型名 / 大小文案', () => {
  const pdf = describeAttachment('发票影像.pdf', 'application/pdf', 1_536_000);
  assert.equal(pdf.kind, 'pdf');
  assert.equal(pdf.icon, 'file-pdf');
  assert.equal(pdf.theme, 'danger');
  assert.equal(pdf.kindLabel, 'PDF');
  assert.equal(pdf.sizeLabel, '1.5 MB');

  const other = describeAttachment('没有扩展名', null, 512);
  assert.equal(other.kind, 'other');
  assert.equal(other.icon, 'file-unknown');
  assert.equal(other.sizeLabel, '512 B');
  assert.equal(describeAttachment('照片.PNG', null, 2048).sizeLabel, '2 KB');
});

// ==================== ③ 手动排序：重排纯函数 ====================

test('moveWithin：上移 / 下移 / 边界不动，且不改入参', () => {
  const list = ['a', 'b', 'c'];
  assert.deepEqual(moveWithin(list, 1, 0), ['b', 'a', 'c'], '上移');
  assert.deepEqual(moveWithin(list, 1, 2), ['a', 'c', 'b'], '下移');
  assert.deepEqual(moveWithin(list, 0, 2), ['b', 'c', 'a'], '跨位移动');
  assert.deepEqual(list, ['a', 'b', 'c'], '入参不能被改（面板里 items 是响应式数组）');

  // 边界：越界 / 非整数 / 原地 → 原样副本
  assert.deepEqual(moveWithin(list, 0, -1), ['a', 'b', 'c']);
  assert.deepEqual(moveWithin(list, 2, 3), ['a', 'b', 'c']);
  assert.deepEqual(moveWithin(list, 0, 0), ['a', 'b', 'c']);
  assert.deepEqual(moveWithin(list, 0.5, 1), ['a', 'b', 'c']);
  assert.deepEqual(moveWithin([], 0, 1), []);
  // 单个元素：怎么移都不变
  assert.deepEqual(moveWithin(['only'], 0, 0), ['only']);
});

// ==================== ④ 面板 / 接口契约（源码级断言） ====================

test('面板用统一的大小格式化与图标映射（不许各写一份）', () => {
  const panel = readSource('views/business/erp/shared/DocAttachmentPanel.vue');
  assert.ok(panel.includes('describeAttachment'), '面板必须用 describeAttachment 算图标/大小');
  assert.ok(panel.includes('moveWithin'), '面板必须用 moveWithin 做上移/下移');
  assert.ok(panel.includes('sortDocAttachments'), '面板必须调用 sortDocAttachments 落库');
  assert.ok(
    !panel.includes('formatFileSize'),
    '面板不许再用全局 formatFileSize（2 位小数口径与本卡 1 位小数冲突）',
  );
  // 卡面七列
  for (const column of ['文件名', '大小', '上传人', '上传时间', '备注', '操作', '类型']) {
    assert.ok(panel.includes(column), `表头缺列：${column}`);
  }
  // e2e 定位点（保持与既有脚本一致：*-attach-item）
  for (const testid of ['-item', '-icon', '-size', '-up', '-down', '-sort']) {
    assert.ok(panel.includes(testid), `缺 data-testid 后缀：${testid}`);
  }
});

test('接口契约：list 带 sort 参数，sort 接口带 sort_order 与完整 ids', () => {
  const api = readSource('api/business/erp/attachment.ts');
  assert.ok(api.includes('sort_order: number'), '出参类型必须声明 sort_order');
  assert.ok(api.includes("sort: DocAttachmentSortMode = 'time'"), 'list 必须传 sort，缺省 time');
  assert.ok(api.includes("'/api/business/attachment/sort'"), 'sort 接口必须写全 /api 前缀');
  assert.ok(api.includes('ids: string[]'), 'sort 接口必须提交完整 id 顺序');
  assert.ok(api.includes('DocAttachmentSortMode'), '必须声明排序口径类型');
});
