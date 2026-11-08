/**
 * 单据附件的**上传上限 + 下载引导**纯逻辑（ATT-05）。
 *
 * # 为什么单独一层
 *
 * 与 `attachment-panel-pure.ts` / `attachment-preview-pure.ts` 同款做法：能抽出来的
 * 「按大小分流」「超限文案」「下载后提示哪种打开方式」全部放这里，不依赖 Vue / DOM /
 * 路径别名（`@/…` 在 `node:test` 下解析不了），因此 `pnpm test:unit` 能直接跑。
 *
 * # 口径（用户拍板，ATT-05）
 *
 * | 规则 | 值 |
 * |------|-----|
 * | 单文件上限 | **20 MB**（前端拦截，后端同样拦；绕过前端直传后端给 400） |
 * | 超限文案 | 「文件超过 20MB，请压缩或分卷上传」 |
 * | Office（doc/docx/xls/xlsx/ppt/pptx/wps/et/dps） | **不预览**，只给下载，下载后提示用本地 Office/WPS 打开 |
 *
 * ⚠ `MAX_ATTACHMENT_FILE_BYTES` / `FILE_TOO_LARGE_MESSAGE` / `OFFICE_OPEN_HINT` 三处
 * 文案与数字由后端单测 `business::attachment::tests::file_size_limit_matches_frontend`
 * 逐字比对（`backend/src/business/attachment/dto.rs` 的 `MAX_FILE_BYTES` /
 * `FILE_TOO_LARGE_MSG`）—— 改这里必须同步改后端，否则 cargo test 直接红。
 */

import {
  isPreviewable,
  resolvePreviewKind,
  type PreviewKind,
} from './attachment-preview-pure';

/**
 * 单文件上限：20 MiB（与后端 `dto::MAX_FILE_BYTES` 同一数字）。
 *
 * 用 `1024 * 1024` 而不是 `20_000_000`：后端按二进制 MB 判（`bytes > 20 * 1024 * 1024`），
 * 前端用十进制 MB 会放行 20.9 MB 的文件让后端 400 打脸。
 */
export const MAX_ATTACHMENT_FILE_BYTES = 20 * 1024 * 1024;

/** 上限的 MB 数值（文案用，避免 `20` 写两遍） */
export const MAX_ATTACHMENT_FILE_MB = 20;

/** 超限提示（与后端 `FILE_TOO_LARGE_MSG` 逐字一致） */
export const FILE_TOO_LARGE_MESSAGE = '文件超过 20MB，请压缩或分卷上传';

/** Office 文档下载后的引导文案（卡面逐字：「已下载到文件夹，请用本地 Office/WPS 打开」） */
export const OFFICE_OPEN_HINT = '已下载到文件夹，请用本地 Office/WPS 打开';

/** 压缩包下载后的引导文案（解压是本机动作，没有在线入口） */
export const ARCHIVE_OPEN_HINT = '已下载到文件夹，请解压后查看';

/** 其他类型（不认识）下载后的引导文案 */
export const GENERIC_OPEN_HINT = '已下载到文件夹，请用本地应用打开';

/** 上传前只需要「文件名 + 字节数」两个字段（`File` 天然满足） */
export interface SizedFileLike {
  name: string;
  size: number;
}

/** 单文件是否超限（非有限数 / 负数按「不超限」处理，交给后端兜底） */
export function isFileTooLarge(size: number): boolean {
  if (!Number.isFinite(size) || size < 0) {
    return false;
  }
  return size > MAX_ATTACHMENT_FILE_BYTES;
}

/**
 * 按 20 MB 把一批文件分成「能传的」和「超限的」（保序，不改入参）。
 *
 * 为什么不在 `upload()` 里 `if (file.size > ...)` 一把梭：
 * 一个入口一次可能选 10 个文件，逐个判 + 逐个弹提示会刷屏；这里一次分好，
 * 界面只弹**一条**汇总提示（见 [`tooLargeMessage`]）。
 */
export function partitionFilesBySize<T extends SizedFileLike>(
  files: readonly T[],
): { accepted: T[]; rejected: T[] } {
  const accepted: T[] = [];
  const rejected: T[] = [];
  for (const file of files) {
    if (isFileTooLarge(file.size)) {
      rejected.push(file);
    } else {
      accepted.push(file);
    }
  }
  return { accepted, rejected };
}

/**
 * 超限提示文案：固定句子 + 括号里点名是哪些文件（一个都没有时返回 `null`）。
 *
 * 句子部分**逐字**是 `FILE_TOO_LARGE_MESSAGE`（卡面文案），文件名只作补充，
 * 这样无论 1 个还是 N 个超限，用户看到的都是同一句话开头。
 */
export function tooLargeMessage(rejected: readonly SizedFileLike[]): string | null {
  if (rejected.length === 0) {
    return null;
  }
  const names = rejected.map((file) => file.name || '未命名文件');
  // 名字太多时只点前 3 个，避免提示框被文件名撑满（但仍说清「还有 N 个」）
  const head = names.slice(0, 3).join('、');
  const tail = names.length > 3 ? ` 等 ${names.length} 个文件` : '';
  return `${FILE_TOO_LARGE_MESSAGE}（${head}${tail}）`;
}

/** 是否给「预览」按钮（图片 / PDF 才给；Office / 压缩包 / 其他一律只下载） */
export function canPreviewFile(fileName: string, mimeType?: string | null): boolean {
  return isPreviewable(resolvePreviewKind(fileName, mimeType));
}

/**
 * 下载完成后的提示文案。
 *
 * # 返回值
 *
 * - 图片 / PDF（可预览）→ `null`：下载只是「另存一份」，不需要教用户怎么打开；
 * - Office → [`OFFICE_OPEN_HINT`]（用户拍板：不做在线预览，只能本地 Office/WPS 打开）；
 * - 压缩包 → [`ARCHIVE_OPEN_HINT`]；
 * - 其他 → [`GENERIC_OPEN_HINT`]。
 *
 * # 为什么按 `PreviewKind` 判而不是再列一遍扩展名
 *
 * 「预览方式」与「打开方式」是同一个判断的两种后果，两处各列一份扩展名必然漂移
 * （加个 `.ofd` 只改了预览、忘了改下载提示）。
 */
export function downloadHintFor(fileName: string, mimeType?: string | null): string | null {
  const kind: PreviewKind = resolvePreviewKind(fileName, mimeType);
  if (isPreviewable(kind)) {
    return null;
  }
  if (kind === 'office') {
    return OFFICE_OPEN_HINT;
  }
  if (kind === 'archive') {
    return ARCHIVE_OPEN_HINT;
  }
  return GENERIC_OPEN_HINT;
}
