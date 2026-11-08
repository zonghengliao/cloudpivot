/**
 * 附件在线预览的**纯逻辑**（ATT-01）。
 *
 * # 为什么单独一层
 *
 * `AttachmentPreview.vue` 里能抽出「按扩展名判预览方式」「缩放/旋转/页码夹取」这类
 * 纯函数的部分全部放这里：不依赖 Vue / DOM / 路径别名（`@/…` 在 `node:test` 下解析不了，
 * 见 `tests/unit/*.test.ts` 的既有做法），因此可以被 `pnpm test:unit` 直接跑。
 *
 * # 口径（用户拍板）
 *
 * | 类型 | 扩展名 | 预览方式 |
 * |------|--------|----------|
 * | 图片 | jpg/jpeg/png/webp/gif/bmp | `<img>` + 滚轮缩放 + 旋转 + 左右切换 |
 * | PDF  | pdf | pdf.js 画布渲染 + 翻页 |
 * | 办公 | doc/docx/xls/xlsx/ppt/pptx/wps/et/dps | **不预览**，图标 + 下载 |
 * | 压缩包 | zip/rar/7z/gz/tar/bz2/xz | 不预览，只下载 |
 * | 其他 | 其余 | 不预览，只下载 |
 *
 * 判类型以**扩展名优先**，没扩展名时才退回 MIME —— 与后端
 * `support/file/handler.rs` 的上传白名单（按扩展名校验）保持同一套口径，
 * 避免「后端按扩展名存、前端按 MIME 判」两边漂移。
 */

/** 预览方式 */
export type PreviewKind = 'image' | 'pdf' | 'office' | 'archive' | 'other';

/** 图片扩展名（小写，无点） */
export const IMAGE_EXTENSIONS = ['jpg', 'jpeg', 'png', 'webp', 'gif', 'bmp'] as const;

/** PDF 扩展名 */
export const PDF_EXTENSIONS = ['pdf'] as const;

/**
 * 办公文档扩展名。
 *
 * `wps` / `et` / `dps` 是 WPS Office 自家的三件套（文字 / 表格 / 演示），
 * 与 doc/xls/ppt 等价对待 —— 一律不预览，只下载。
 */
export const OFFICE_EXTENSIONS = [
  'doc',
  'docx',
  'xls',
  'xlsx',
  'ppt',
  'pptx',
  'wps',
  'et',
  'dps',
] as const;

/** 压缩包扩展名 */
export const ARCHIVE_EXTENSIONS = ['zip', 'rar', '7z', 'gz', 'tar', 'bz2', 'xz'] as const;

/** 缩放上下限（再大像素糊、再小看不清） */
export const ZOOM_MIN = 0.25;
export const ZOOM_MAX = 5;
/** 按钮缩放的步长 */
export const ZOOM_STEP = 0.25;
/** 滚轮缩放的步长（比按钮细，滚起来才跟手） */
export const WHEEL_ZOOM_STEP = 0.1;
/** 旋转步长（每次 90°） */
export const ROTATE_STEP = 90;

/** 预览面板展示的一条附件（两种来源：`sys_file` / 单据附件，由调用方抹平差异）。 */
export interface PreviewItem {
  /** 列表 key（同一份文件可能被多个单据引用，不能直接用 fileId 当 key） */
  key: string;
  /** 文件 id（`sys_file.id`），未给 `load` 时用它走默认下载接口 */
  fileId: string;
  /** 展示文件名（带扩展名，用来判预览方式） */
  name: string;
  /** 字节数 */
  size: number;
  /** MIME（扩展名缺失时的兜底判据，也可为空） */
  mimeType?: string | null;
  /** 上传人展示名 */
  uploader?: string | null;
  /** 上传时间（ISO / `YYYY-MM-DD HH:mm:ss` 均可） */
  uploadedAt?: string | null;
  /** 备注 */
  remark?: string | null;
  /** 业务标签（如单据附件的「合同扫描件」） */
  tag?: string | null;
  /**
   * 自定义取字节函数。
   *
   * 单据附件走 `business/attachment/download/{id}`（入参是**绑定行** id，
   * 不是 `file_id`），所以不能只靠 `fileId` 拼 URL；不传则默认走
   * `support/file/download/{fileId}`。
   */
  load?: () => Promise<Blob>;
}

/** 取小写扩展名（不含点）；无扩展名返回空串 */
export function extensionOf(fileName: string): string {
  const name = (fileName ?? '').trim();
  if (!name) {
    return '';
  }
  const dot = name.lastIndexOf('.');
  // 隐藏文件（`.env`）与结尾带点（`a.`）都不算扩展名
  if (dot <= 0 || dot === name.length - 1) {
    return '';
  }
  return name.slice(dot + 1).toLowerCase();
}

/** MIME 粗分类（仅在扩展名缺失或不认识时兜底） */
function kindFromMime(mimeType?: string | null): PreviewKind {
  const mime = (mimeType ?? '').toLowerCase();
  if (!mime) {
    return 'other';
  }
  if (mime.startsWith('image/')) {
    return 'image';
  }
  if (mime === 'application/pdf') {
    return 'pdf';
  }
  if (
    mime.includes('msword') ||
    mime.includes('officedocument') ||
    mime.includes('ms-excel') ||
    mime.includes('ms-powerpoint') ||
    mime.includes('wps')
  ) {
    return 'office';
  }
  if (mime.includes('zip') || mime.includes('rar') || mime.includes('7z') || mime.includes('tar')) {
    return 'archive';
  }
  return 'other';
}

/**
 * 判定预览方式。
 *
 * 扩展名优先（与后端上传白名单同口径）；扩展名不认识时用 MIME 兜底；
 * 都对不上就是 `other`（只下载）。
 */
export function resolvePreviewKind(fileName: string, mimeType?: string | null): PreviewKind {
  const ext = extensionOf(fileName);
  if (ext) {
    if ((IMAGE_EXTENSIONS as readonly string[]).includes(ext)) {
      return 'image';
    }
    if ((PDF_EXTENSIONS as readonly string[]).includes(ext)) {
      return 'pdf';
    }
    if ((OFFICE_EXTENSIONS as readonly string[]).includes(ext)) {
      return 'office';
    }
    if ((ARCHIVE_EXTENSIONS as readonly string[]).includes(ext)) {
      return 'archive';
    }
    // 扩展名存在但不在任何清单里：交给 MIME 兜底（如 .svg 的 image/svg+xml）
    return kindFromMime(mimeType);
  }
  return kindFromMime(mimeType);
}

/** 是否支持在线预览（图片 / PDF） */
export function isPreviewable(kind: PreviewKind): boolean {
  return kind === 'image' || kind === 'pdf';
}

/** 不预览时的提示文案（用户拍板：Office 一律只下载） */
export function unsupportedHint(kind: PreviewKind): string {
  if (kind === 'office') {
    return 'Office 文档不提供在线预览，请下载后查看';
  }
  if (kind === 'archive') {
    return '压缩包不提供在线预览，请下载后解压查看';
  }
  return '该类型文件不提供在线预览，请下载后查看';
}

/** 预览方式的中文名（右侧元信息里展示） */
export function kindLabel(kind: PreviewKind): string {
  if (kind === 'image') {
    return '图片预览';
  }
  if (kind === 'pdf') {
    return 'PDF 预览';
  }
  return '仅下载';
}

/** 四舍五入到两位小数，避免 0.1 累加出 1.2000000000000002 这种脏值 */
function round2(value: number): number {
  return Math.round(value * 100) / 100;
}

/** 缩放值夹取到 [ZOOM_MIN, ZOOM_MAX]，非法值按 1 处理 */
export function clampZoom(zoom: number): number {
  if (!Number.isFinite(zoom)) {
    return 1;
  }
  return round2(Math.min(ZOOM_MAX, Math.max(ZOOM_MIN, zoom)));
}

/** 在当前缩放上加减一步（button / 滚轮共用，delta 可以为负） */
export function zoomBy(current: number, delta: number): number {
  return clampZoom((Number.isFinite(current) ? current : 1) + delta);
}

/** 缩放百分比文案，如 `125%` */
export function formatZoom(zoom: number): string {
  return `${Math.round(clampZoom(zoom) * 100)}%`;
}

/** 旋转角累加并归一到 [0, 360) */
export function rotateBy(current: number, delta: number = ROTATE_STEP): number {
  const base = Number.isFinite(current) ? current : 0;
  const next = Math.round(base + delta);
  return ((next % 360) + 360) % 360;
}

/** 页码夹取到 [1, total]；total 非法时按 1 页处理 */
export function clampPage(page: number, total: number): number {
  const max = Number.isFinite(total) && total >= 1 ? Math.floor(total) : 1;
  if (!Number.isFinite(page)) {
    return 1;
  }
  return Math.min(max, Math.max(1, Math.floor(page)));
}

/** 图片的 CSS transform：缩放 + 旋转 + 拖拽偏移（先缩放旋转，再平移） */
export function imageTransform(zoom: number, rotation: number, offsetX = 0, offsetY = 0): string {
  return `translate(${Math.round(offsetX)}px, ${Math.round(offsetY)}px) scale(${clampZoom(zoom)}) rotate(${rotateBy(rotation, 0)}deg)`;
}
