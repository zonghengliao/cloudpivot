/**
 * ATT-01 单测：附件在线预览的纯逻辑 + 组件关键约束。
 *
 * # 钉死的口径
 *
 * 1. **判类型以扩展名优先**：与后端上传白名单（`support/file/handler.rs` 按扩展名校验）
 *    同口径；扩展名不认识才退回 MIME。
 * 2. **Office / 压缩包一律不预览**（用户拍板），只给下载 —— 所以 `isPreviewable` 对这两类
 *    必须为 false，界面上也就不会去拉字节。
 * 3. **缩放 / 旋转 / 页码夹取**：滚轮不会把图缩成 0、旋转角必须归一到 [0,360)、
 *    页码不会越界到 0 或 total+1（越界会让 pdf.js 抛错，弹窗里变成一片空白）。
 * 4. **blob URL 必须成对释放**：组件源码里 `createObjectURL` 与 `revokeObjectURL` 必须
 *    同时存在，且**不许**出现裸的 `/api/...` 地址（裸链不带 Authorization → 401，
 *    这是验收 ① 最容易踩空的地方）。
 *
 * 运行：`pnpm test:unit`
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  ARCHIVE_EXTENSIONS,
  IMAGE_EXTENSIONS,
  OFFICE_EXTENSIONS,
  ZOOM_MAX,
  ZOOM_MIN,
  clampPage,
  clampZoom,
  extensionOf,
  formatZoom,
  imageTransform,
  isPreviewable,
  kindLabel,
  resolvePreviewKind,
  rotateBy,
  unsupportedHint,
  zoomBy,
} from '../../src/components/attachment-preview-pure';

const HERE = dirname(fileURLToPath(import.meta.url));
const SRC = resolve(HERE, '../../src');

function readSource(relative: string): string {
  return readFileSync(join(SRC, relative), 'utf8');
}

// ==================== ① 扩展名解析 ====================

test('扩展名解析：小写化、多段取末段、隐藏文件与结尾点都不算扩展名', () => {
  assert.equal(extensionOf('a.JPG'), 'jpg');
  assert.equal(extensionOf('归档.tar.gz'), 'gz');
  assert.equal(extensionOf('.env'), '');
  assert.equal(extensionOf('a.'), '');
  assert.equal(extensionOf('  发票.PDF  '), 'pdf');
  assert.equal(extensionOf('没有扩展名'), '');
  assert.equal(extensionOf(''), '');
});

// ==================== ② 预览方式分流 ====================

test('图片：jpg/jpeg/png/webp/gif/bmp 都走 img 预览（大小写不敏感）', () => {
  for (const ext of IMAGE_EXTENSIONS) {
    assert.equal(resolvePreviewKind(`a.${ext}`), 'image', `${ext} 应判为图片`);
    assert.equal(resolvePreviewKind(`a.${ext.toUpperCase()}`), 'image', `${ext} 大写也要认`);
  }
});

test('PDF：只有 pdf 走 pdf.js', () => {
  assert.equal(resolvePreviewKind('报价单.pdf'), 'pdf');
  assert.equal(resolvePreviewKind('报价单.PDF'), 'pdf');
  assert.equal(isPreviewable('pdf'), true);
});

test('Office（doc/docx/xls/xlsx/ppt/pptx/wps/et/dps）→ 不预览', () => {
  for (const ext of OFFICE_EXTENSIONS) {
    const kind = resolvePreviewKind(`a.${ext}`);
    assert.equal(kind, 'office', `${ext} 应判为 office`);
    assert.equal(isPreviewable(kind), false, `${ext} 不该在线预览`);
  }
  // 用户点名的三个
  assert.equal(resolvePreviewKind('合同.docx'), 'office');
  assert.equal(resolvePreviewKind('对账表.xlsx'), 'office');
  assert.equal(resolvePreviewKind('说明.wps'), 'office');
});

test('压缩包 → 不预览，只下载', () => {
  for (const ext of ARCHIVE_EXTENSIONS) {
    const kind = resolvePreviewKind(`a.${ext}`);
    assert.equal(kind, 'archive', `${ext} 应判为 archive`);
    assert.equal(isPreviewable(kind), false);
  }
});

test('无扩展名才用 MIME 兜底；扩展名认识时 MIME 不参与判定', () => {
  assert.equal(resolvePreviewKind('blob', 'image/png'), 'image');
  assert.equal(resolvePreviewKind('blob', 'application/pdf'), 'pdf');
  assert.equal(
    resolvePreviewKind('blob', 'application/vnd.openxmlformats-officedocument.spreadsheetml.sheet'),
    'office',
  );
  assert.equal(resolvePreviewKind('blob', 'application/vnd.ms-excel'), 'office');
  assert.equal(resolvePreviewKind('blob', 'application/zip'), 'archive');
  assert.equal(resolvePreviewKind('blob', ''), 'other');
  assert.equal(resolvePreviewKind('blob', 'text/plain'), 'other');
  // 扩展名不认识 → 交给 MIME
  assert.equal(resolvePreviewKind('a.unknownext', 'image/webp'), 'image');
  assert.equal(resolvePreviewKind('a.unknownext', 'application/octet-stream'), 'other');
});

test('提示文案：Office / 压缩包都明确指向「下载」', () => {
  assert.match(unsupportedHint('office'), /下载/);
  assert.match(unsupportedHint('archive'), /压缩包/);
  assert.match(unsupportedHint('other'), /下载/);
  assert.equal(kindLabel('image'), '图片预览');
  assert.equal(kindLabel('pdf'), 'PDF 预览');
  assert.equal(kindLabel('office'), '仅下载');
});

// ==================== ③ 缩放 / 旋转 / 页码 ====================

test('缩放夹取在 [ZOOM_MIN, ZOOM_MAX]，非法值按 1 处理', () => {
  assert.equal(clampZoom(0.01), ZOOM_MIN);
  assert.equal(clampZoom(99), ZOOM_MAX);
  assert.equal(clampZoom(1.234), 1.23);
  assert.equal(clampZoom(Number.NaN), 1);
  assert.equal(clampZoom(Number.POSITIVE_INFINITY), 1);
});

test('加减步长不会越界，也不会累积浮点脏值', () => {
  assert.equal(zoomBy(1, 0.25), 1.25);
  assert.equal(zoomBy(1, -0.1), 0.9);
  assert.equal(zoomBy(ZOOM_MIN, -0.25), ZOOM_MIN);
  assert.equal(zoomBy(ZOOM_MAX, 0.25), ZOOM_MAX);
  // 0.1 累加 7 次的经典脏值
  let zoom = 1;
  for (let i = 0; i < 7; i += 1) {
    zoom = zoomBy(zoom, 0.1);
  }
  assert.equal(zoom, 1.7);
});

test('缩放展示为百分比整数', () => {
  assert.equal(formatZoom(1), '100%');
  assert.equal(formatZoom(1.25), '125%');
  assert.equal(formatZoom(0.25), '25%');
  assert.equal(formatZoom(0.255), '26%');
});

test('旋转每次 90° 且归一到 [0,360)', () => {
  assert.equal(rotateBy(0), 90);
  assert.equal(rotateBy(270), 0);
  assert.equal(rotateBy(0, -90), 270);
  assert.equal(rotateBy(180, 180), 0);
  assert.equal(rotateBy(Number.NaN), 90);
});

test('页码夹取在 [1, total]：pdf.js 拿到 0 或 total+1 会直接抛错', () => {
  assert.equal(clampPage(0, 3), 1);
  assert.equal(clampPage(4, 3), 3);
  assert.equal(clampPage(2.9, 3), 2);
  assert.equal(clampPage(Number.NaN, 3), 1);
  assert.equal(clampPage(5, 0), 1);
  assert.equal(clampPage(2, 2), 2);
});

test('图片 transform 同时带平移、缩放与旋转（顺序固定）', () => {
  assert.equal(imageTransform(1, 0), 'translate(0px, 0px) scale(1) rotate(0deg)');
  assert.equal(imageTransform(1.5, 90, 12.4, -8.6), 'translate(12px, -9px) scale(1.5) rotate(90deg)');
});

// ==================== ④ 组件关键约束（读源码钉） ====================

test('组件只用 blob URL 预览：img 的 src 绑定的是 objectUrl，模板里没有裸地址', () => {
  const source = readSource('components/AttachmentPreview.vue');
  // 裸链不带 Authorization → 401，且无权限时拿到 JSON 错误体会变成碎图。
  // 注：文件头注释里为了讲清这个坑会写 `/api/support/file/...` 作反例，
  // 所以这里钉的是「模板里 src/href 全是绑定表达式」。
  assert.equal(source.includes(':src="objectUrl"'), true, '图片必须渲染 blob URL（objectUrl）');
  assert.equal(source.includes('downloadFileBlob'), true, '默认下载入口必须是带鉴权的 downloadFileBlob');
  assert.equal(source.includes('URL.createObjectURL'), true);
  assert.equal(source.includes('URL.revokeObjectURL'), true, '每个 blob URL 都必须有对应的释放');

  const template = source.slice(source.indexOf('<template>'), source.lastIndexOf('</template>'));
  assert.equal(
    /(?:^|[\s(])(?:src|href)\s*=\s*["']/.test(template),
    false,
    '模板里的 src/href 必须全是绑定表达式，不许写死后端地址（会绕过鉴权头）',
  );
});

test('关闭即释放：visible=false 的分支里调用 cleanup（验收 ⑥ 的唯一保证）', () => {
  const source = readSource('components/AttachmentPreview.vue');
  assert.match(source, /if \(open\) \{[\s\S]{0,200}?\n\s*\}\s*\n\s*cleanup\(\);/);
  assert.match(source, /onBeforeUnmount\(\(\) => \{[\s\S]*?cleanup\(\);/);
});

test('图片支持滚轮缩放与旋转；Office/压缩包不进预览分支', () => {
  const source = readSource('components/AttachmentPreview.vue');
  assert.equal(source.includes('@wheel.prevent'), true, '滚轮缩放必须挂 wheel 并 prevent 页面滚动');
  assert.equal(source.includes('onRotate'), true);
  // 预览分支只看 image / pdf，其余落到 fallback（图标 + 下载）
  assert.equal(source.includes("kind === 'image'"), true);
  assert.equal(source.includes("kind === 'pdf'"), true);
  assert.equal(source.includes('attachment-preview-fallback'), true);
});
