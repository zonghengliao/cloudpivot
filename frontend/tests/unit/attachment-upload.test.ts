/**
 * ATT-05 单测：单文件 20 MB 拦截 + Office 只下载不预览 + 下载后引导文案 + 图标配色。
 *
 * # 钉死的口径（卡面验收①③⑤）
 *
 * 1. **20 MB 边界**：正好 20 MiB 放行、20 MiB + 1 字节拒绝（前端与后端同一个数字）；
 * 2. **超限文案逐字**：`文件超过 20MB，请压缩或分卷上传`（后端 `FILE_TOO_LARGE_MSG` 同句）；
 * 3. **Office 不给预览**：doc/docx/xls/xlsx/ppt/pptx/wps/et/dps 的 `previewable === false`，
 *    下载后提示 `已下载到文件夹，请用本地 Office/WPS 打开`；
 * 4. **pdf / 图片不受影响**：仍然 `previewable === true`，下载不弹打开引导；
 * 5. **图标配色**：Word 蓝 / Excel 绿 / PPT 橙 / **WPS 红**；
 * 6. **源码级契约**：面板的「预览」按钮必须挂 `v-if="row.display.previewable"`、
 *    上传必须先 `partitionFilesBySize`、下载必须弹 `downloadHintFor` 的文案 ——
 *    这些是「行为」而不只是「纯函数」，光测纯函数测不到「模板里忘了 v-if」。
 *
 * 运行：`pnpm test:unit`
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  ARCHIVE_OPEN_HINT,
  FILE_TOO_LARGE_MESSAGE,
  GENERIC_OPEN_HINT,
  MAX_ATTACHMENT_FILE_BYTES,
  MAX_ATTACHMENT_FILE_MB,
  OFFICE_OPEN_HINT,
  canPreviewFile,
  downloadHintFor,
  isFileTooLarge,
  partitionFilesBySize,
  tooLargeMessage,
} from '../../src/components/attachment-upload-pure';
import { describeAttachment, WPS_EXTENSIONS, wpsKindLabelOf } from '../../src/components/attachment-panel-pure';

const HERE = dirname(fileURLToPath(import.meta.url));
const SRC = resolve(HERE, '../../src');

function readSource(relative: string): string {
  return readFileSync(join(SRC, relative), 'utf8');
}

const FILE_PANEL = 'views/business/erp/shared/DocAttachmentPanel.vue';

// ==================== ① 20 MB 上限（验收③④的口径） ====================

test('上限：常量是 20 MiB（与后端 dto::MAX_FILE_BYTES 同一个数字）', () => {
  assert.equal(MAX_ATTACHMENT_FILE_BYTES, 20 * 1024 * 1024);
  assert.equal(MAX_ATTACHMENT_FILE_BYTES, 20_971_520);
  assert.equal(MAX_ATTACHMENT_FILE_MB, 20);
});

test('isFileTooLarge：边界逐字节正确（20 MiB 放行、+1 字节拒绝）', () => {
  assert.equal(isFileTooLarge(0), false);
  assert.equal(isFileTooLarge(1024), false);
  assert.equal(isFileTooLarge(MAX_ATTACHMENT_FILE_BYTES - 1), false, '差 1 字节到上限必须放行');
  assert.equal(isFileTooLarge(MAX_ATTACHMENT_FILE_BYTES), false, '正好 20 MB 必须放行（边界不能提前拒绝）');
  assert.equal(isFileTooLarge(MAX_ATTACHMENT_FILE_BYTES + 1), true, '超 1 字节必须拒');
  assert.equal(isFileTooLarge(25 * 1024 * 1024), true, '卡面验收③的 25 MB 必须拒');
  // 脏值不误伤（非有限数 / 负数交给后端兜底，前端不装懂）
  assert.equal(isFileTooLarge(Number.NaN), false);
  assert.equal(isFileTooLarge(-1), false);
  assert.equal(isFileTooLarge(Number.POSITIVE_INFINITY), false);
});

test('partitionFilesBySize：保序分成「能传的 / 超限的」，不改入参', () => {
  const files = [
    { name: '合同扫描件.docx', size: 1_200_000 },
    { name: '发票影像.pdf', size: 25 * 1024 * 1024 },
    { name: '对账单.xlsx', size: MAX_ATTACHMENT_FILE_BYTES },
    { name: '超大.pptx', size: MAX_ATTACHMENT_FILE_BYTES + 1 },
  ];
  const snapshot = files.map((file) => `${file.name}:${file.size}`);

  const { accepted, rejected } = partitionFilesBySize(files);
  assert.deepEqual(
    accepted.map((file) => file.name),
    ['合同扫描件.docx', '对账单.xlsx'],
    '正好 20 MB 的要进 accepted（顺序按原顺序）',
  );
  assert.deepEqual(
    rejected.map((file) => file.name),
    ['发票影像.pdf', '超大.pptx'],
    '25 MB 与 20 MB + 1 字节都要进 rejected',
  );
  assert.deepEqual(files.map((file) => `${file.name}:${file.size}`), snapshot, '入参不能被改');

  // 全通过 / 全拒 / 空数组
  const allOk = partitionFilesBySize([{ name: 'a.pdf', size: 10 }]);
  assert.equal(allOk.accepted.length, 1);
  assert.equal(allOk.rejected.length, 0);
  const allBad = partitionFilesBySize([{ name: 'b.pdf', size: 30 * 1024 * 1024 }]);
  assert.equal(allBad.accepted.length, 0);
  assert.equal(allBad.rejected.length, 1);
  const empty = partitionFilesBySize([]);
  assert.deepEqual([empty.accepted.length, empty.rejected.length], [0, 0]);
  assert.equal(tooLargeMessage(empty.rejected), null, '没有超限文件时不弹提示');
});

test('tooLargeMessage：句子部分逐字是「文件超过 20MB，请压缩或分卷上传」', () => {
  assert.equal(FILE_TOO_LARGE_MESSAGE, '文件超过 20MB，请压缩或分卷上传');

  const one = tooLargeMessage([{ name: '合同.docx', size: 25 * 1024 * 1024 }]) ?? '';
  assert.ok(one.length > 0, '超限必须有提示');
  assert.ok(one.startsWith(FILE_TOO_LARGE_MESSAGE), `提示必须以卡面文案开头：${one}`);
  assert.ok(one.includes('合同.docx'), '要点名是哪个文件');

  // 多个超限：只点前 3 个 + 「等 N 个文件」，不能把提示撑爆
  const many =
    tooLargeMessage(
      ['a.docx', 'b.xlsx', 'c.pptx', 'd.wps', 'e.pdf'].map((name) => ({
        name,
        size: 25 * 1024 * 1024,
      })),
    ) ?? '';
  assert.ok(many.length > 0);
  assert.ok(many.includes('a.docx、b.xlsx、c.pptx'), many);
  assert.ok(many.includes('等 5 个文件'), many);
  assert.ok(!many.includes('d.wps'), '第 4 个起不点名（只报总数）');

  // 没有文件名时也不能出现空括号内容
  const nameless = tooLargeMessage([{ name: '', size: 25 * 1024 * 1024 }]) ?? '';
  assert.ok(nameless.includes('未命名文件'), nameless);
});

// ==================== ② Office 只下载（验收①⑤） ====================

test('Office 九种扩展名一律不给预览（doc/docx/xls/xlsx/ppt/pptx/wps/et/dps）', () => {
  const office = ['doc', 'docx', 'xls', 'xlsx', 'ppt', 'pptx', 'wps', 'et', 'dps'];
  for (const ext of office) {
    assert.equal(canPreviewFile(`合同.${ext}`, null), false, `${ext} 不许给「预览」按钮`);
    assert.equal(
      canPreviewFile(`合同.${ext.toUpperCase()}`, null),
      false,
      `${ext} 大写扩展名也不许预览`,
    );
  }
});

test('pdf / 图片仍然可预览（验收⑤：不受影响）', () => {
  for (const ext of ['pdf', 'jpg', 'jpeg', 'png', 'webp', 'gif', 'bmp']) {
    assert.equal(canPreviewFile(`影像.${ext}`, null), true, `${ext} 必须还能预览`);
  }
  // MIME 兜底：没有扩展名时按 MIME 判
  assert.equal(canPreviewFile('无扩展名', 'application/pdf'), true);
  assert.equal(canPreviewFile('无扩展名', 'image/png'), true);
  // 压缩包 / 其他：不预览
  assert.equal(canPreviewFile('归档.zip', null), false);
  assert.equal(canPreviewFile('说明.txt', null), false);
});

test('downloadHintFor：Office 提示本地 Office/WPS 打开，图片 / PDF 不弹', () => {
  assert.equal(OFFICE_OPEN_HINT, '已下载到文件夹，请用本地 Office/WPS 打开');
  for (const ext of ['doc', 'docx', 'xls', 'xlsx', 'ppt', 'pptx', 'wps', 'et', 'dps']) {
    assert.equal(
      downloadHintFor(`合同.${ext}`, null),
      OFFICE_OPEN_HINT,
      `${ext} 下载后必须提示用本地 Office/WPS 打开`,
    );
  }
  // pdf / 图片：下载只是另存一份，不弹引导
  assert.equal(downloadHintFor('发票.pdf', 'application/pdf'), null);
  assert.equal(downloadHintFor('照片.png', 'image/png'), null);
  // 压缩包 / 其他各有自己的文案（不能都套 Office 那句）
  assert.equal(downloadHintFor('归档.zip', null), ARCHIVE_OPEN_HINT);
  assert.equal(downloadHintFor('说明.txt', null), GENERIC_OPEN_HINT);
  assert.notEqual(ARCHIVE_OPEN_HINT, OFFICE_OPEN_HINT);
  assert.notEqual(GENERIC_OPEN_HINT, OFFICE_OPEN_HINT);
});

// ==================== ③ 图标配色（Word 蓝 / Excel 绿 / PPT 橙 / WPS 红） ====================

test('图标配色：Word 蓝 / Excel 绿 / PPT 橙 / WPS 红', () => {
  const word = describeAttachment('合同.docx', null, 1024);
  assert.equal(word.kind, 'word');
  assert.equal(word.icon, 'file-word');
  assert.equal(word.theme, 'primary', 'Word 走蓝（primary）');
  assert.equal(word.kindLabel, 'Word');

  const excel = describeAttachment('对账.xlsx', null, 1024);
  assert.equal(excel.icon, 'file-excel');
  assert.equal(excel.theme, 'success', 'Excel 走绿（success）');

  const ppt = describeAttachment('汇报.pptx', null, 1024);
  assert.equal(ppt.icon, 'file-powerpoint');
  assert.equal(ppt.theme, 'warning', 'PPT 走橙（warning）');

  // WPS 三件套：形状图标沿用对应家族，颜色统一红，文案点明 WPS
  const expected: [string, string, string][] = [
    ['wps', 'file-word', 'WPS 文字'],
    ['et', 'file-excel', 'WPS 表格'],
    ['dps', 'file-powerpoint', 'WPS 演示'],
  ];
  for (const [ext, icon, label] of expected) {
    const display = describeAttachment(`合同.${ext}`, null, 1024);
    assert.equal(display.icon, icon, `${ext} 的形状图标`);
    assert.equal(display.theme, 'danger', `${ext} 必须是红色（WPS 家族）`);
    assert.equal(display.kindLabel, label, `${ext} 的中文名`);
    assert.equal(display.previewable, false, `${ext} 不许预览`);
  }
  assert.deepEqual([...WPS_EXTENSIONS], ['wps', 'et', 'dps']);
  assert.equal(wpsKindLabelOf('合同.docx'), undefined, '非 WPS 扩展名不给 WPS 文案');
});

test('describeAttachment.previewable：与 canPreviewFile 同一口径', () => {
  for (const name of ['合同.docx', '发票.pdf', '照片.png', '归档.zip', '说明.txt', '模板.wps']) {
    assert.equal(
      describeAttachment(name, null, 100).previewable,
      canPreviewFile(name, null),
      `${name} 的 previewable 必须与 canPreviewFile 一致（面板只认 previewable）`,
    );
  }
});

// ==================== ④ 源码级契约（模板里真的按 previewable 渲染） ====================

test('面板：「预览」按钮挂 v-if="row.display.previewable"，不 previewable 只有「下载」', () => {
  const panel = readSource(FILE_PANEL);
  assert.ok(
    panel.includes('v-if="row.display.previewable"'),
    '「预览」按钮必须按 row.display.previewable 渲染 —— 否则 Office 也会出现预览按钮（验收①）',
  );
  // 预览 / 下载是两个独立按钮，各带 data-testid；下载**无条件**渲染（所有类型都能下载）
  const previewAt = panel.indexOf('`${testidPrefix}-preview`');
  const downloadAt = panel.indexOf('`${testidPrefix}-download`');
  assert.ok(previewAt > 0, '「预览」按钮必须有 data-testid（e2e 要能点到）');
  assert.ok(downloadAt > previewAt, '「下载」按钮在预览之后（同一操作组，顺序固定）');
  // v-if 只挂在预览按钮上：预览按钮到下载按钮之间不能出现第二个 v-if（下载不许被条件挡住）
  const between = panel.slice(previewAt, downloadAt);
  assert.ok(
    !/v-if/.test(between),
    '下载按钮（以及中间部分）不许再有 v-if —— 否则某些类型连下载都点不到',
  );
  // 下载按钮必须传文件信息（下载后要靠文件名判打开方式）
  assert.ok(
    panel.includes('@click="download(row.item)"'),
    '下载按钮必须调 download(row.item)（提示文案要靠 file_name / mime_type）',
  );
});

test('面板：上传前用 partitionFilesBySize 分流，超限提示用 tooLargeMessage', () => {
  const panel = readSource(FILE_PANEL);
  assert.ok(
    panel.includes('partitionFilesBySize'),
    'upload() 必须先按 20 MB 分流（前端拦截，卡面验收③）',
  );
  assert.ok(panel.includes('tooLargeMessage'), '超限提示必须用统一文案函数（不许各写一句）');
  assert.ok(
    panel.includes('MAX_ATTACHMENT_FILE_MB'),
    '拖拽区提示的「单文件上限 N MB」必须用常量（不许写死 100 / 20）',
  );
  assert.ok(!panel.includes('100 MB'), '面板不许再出现 100 MB（上限已改 20 MB）');
  // 上传拦截必须在发请求之前（否则等于没拦）
  const partitionAt = panel.indexOf('partitionFilesBySize');
  const requestAt = panel.indexOf('uploadDocAttachment(props.docType');
  assert.ok(partitionAt > 0 && requestAt > partitionAt, '分流必须发生在上传请求之前');
});

test('面板：下载后弹 downloadHintFor 的文案（Office → 本地 Office/WPS 打开）', () => {
  const panel = readSource(FILE_PANEL);
  assert.ok(panel.includes('downloadHintFor'), 'download() 必须用 downloadHintFor 决定提示');
  const saveAt = panel.indexOf('saveBlob(blob, item.file_name)');
  const hintAt = panel.indexOf('downloadHintFor(item.file_name');
  assert.ok(saveAt > 0 && hintAt > saveAt, '提示必须在 saveBlob 之后（下载完成才引导）');
});

test('预览弹窗也走同一份下载提示（后台文件管理页同样能用到）', () => {
  const preview = readSource('components/AttachmentPreview.vue');
  assert.ok(
    preview.includes("import { downloadHintFor } from './attachment-upload-pure'"),
    '预览弹窗的下载必须复用 attachment-upload-pure 的 downloadHintFor（不许各写一句文案）',
  );
  assert.ok(
    preview.includes('downloadHintFor(item.name'),
    '预览弹窗下载完必须弹引导文案（Office → 本地 Office/WPS 打开）',
  );
});

test('接口层：attachment.ts 不再宣称「前端不做大小校验」（上限已双端校验）', () => {
  const api = readSource('api/business/erp/attachment.ts');
  assert.ok(api.includes('20 MB'), '上传接口注释必须写明单文件 20 MB');
  assert.ok(
    !api.includes('前端不做二次校验'),
    '旧注释「前端不做二次校验」必须改掉（现在前端也拦 20 MB）',
  );
});
