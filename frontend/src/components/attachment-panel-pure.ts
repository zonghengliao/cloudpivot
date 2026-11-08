/**
 * 单据附件面板的**纯逻辑**（ATT-03：列表增强）。
 *
 * # 为什么单独一层
 *
 * 面板里能抽出来的「按扩展名判类型 / 大小格式化 / 重排」全部放这里：不依赖 Vue / DOM /
 * 路径别名（`@/…` 在 `node:test` 下解析不了），因此可以被 `pnpm test:unit` 直接跑。
 * 同款做法见 `attachment-preview-pure.ts`（ATT-01）。
 *
 * # 与全局 `utils/format.formatFileSize` 的关系（**刻意不合并**）
 *
 * 全局那份保留 2 位小数（`1.46 MB`），是给「文件管理 / 导入任务」用的既有口径；
 * ATT-03 卡面明确要求附件列表 `1.5 MB` 这种 1 位小数。两处口径不同是**需求**，
 * 不是遗漏 —— 合并会把别的页面显示一起改掉。若将来要统一，改这里 + 卡面确认。
 */

import { extensionOf, isPreviewable, resolvePreviewKind } from './attachment-preview-pure';

/** 附件文件类型（图标 / 文案的口径，与卡面 pdf/word/excel/ppt/image/zip/other 逐字一致） */
export type AttachmentFileKind = 'pdf' | 'word' | 'excel' | 'ppt' | 'image' | 'zip' | 'other';

/** 各类型可识别的扩展名（小写、不含点） */
export const KIND_EXTENSIONS: Record<AttachmentFileKind, readonly string[]> = {
  pdf: ['pdf'],
  word: ['doc', 'docx', 'wps'],
  excel: ['xls', 'xlsx', 'csv', 'et'],
  ppt: ['ppt', 'pptx', 'dps'],
  image: ['jpg', 'jpeg', 'png', 'webp', 'gif', 'bmp', 'svg'],
  zip: ['zip', 'rar', '7z', 'gz', 'tar', 'bz2', 'xz'],
  other: [],
};

/**
 * WPS 三件套扩展名（ATT-05 配色口径：Word 蓝 / Excel 绿 / PPT 橙 / **WPS 红**）。
 *
 * # 为什么不是第八个 `kind`
 *
 * `wps` / `et` / `dps` 与 `doc` / `xls` / `ppt` 是**同一种文档**（文字 / 表格 / 演示），
 * 形状图标（`file-word` / `file-excel` / `file-powerpoint`）完全一样，用户一眼就能看出
 * 「这是文字文档」；真正的差别在**软件归属**，所以只换颜色（红）与文案（`WPS 文字` / …），
 * 不新增 kind —— 新增会让 `KIND_EXTENSIONS` 多一层交叉，`fileKindOf` 的优先级规则也得重写。
 */
export const WPS_EXTENSIONS = ['wps', 'et', 'dps'] as const;

/** WPS 三件套的中文名（键是扩展名；非 WPS 扩展名查不到 → `undefined`） */
export const WPS_KIND_LABELS: Record<string, string> = {
  wps: 'WPS 文字',
  et: 'WPS 表格',
  dps: 'WPS 演示',
};

/**
 * 是否是 WPS 家族的某个文件；返回它的中文名（不是 WPS 家族则 `undefined`）。
 *
 * 只看**扩展名**（与 `fileKindOf` 同一口径），不认 MIME：`.wps` 的 MIME 各家给得五花八门
 * （`application/kswps` / `application/vnd.ms-works` / 空），靠它判会把 WPS 文档当成「其他」。
 */
export function wpsKindLabelOf(fileName: string): string | undefined {
  const ext = extensionOf(fileName);
  return WPS_KIND_LABELS[ext];
}

/**
 * TDesign 图标名（`t-icon name`）。
 *
 * ⚠ 名字**必须**在 `tdesign-icons-vue-next` 里真实存在：写错不会报错，只会渲染成空白
 * （AGENTS.md「高频踩坑」第二条）。`tests/unit/attachment-panel.test.ts` 会逐个回查
 * `node_modules/tdesign-icons-vue-next/esm/components/<icon>.js` 是否存在。
 */
export const KIND_ICONS: Record<AttachmentFileKind, string> = {
  pdf: 'file-pdf',
  word: 'file-word',
  excel: 'file-excel',
  ppt: 'file-powerpoint',
  image: 'file-image',
  zip: 'file-zip',
  other: 'file-unknown',
};

/** 图标配色（TDesign `theme`）；`default` 是灰，用来让「其他」不抢眼 */
export const KIND_THEMES: Record<AttachmentFileKind, 'danger' | 'primary' | 'success' | 'warning' | 'default'> =
  {
    pdf: 'danger',
    word: 'primary',
    excel: 'success',
    ppt: 'warning',
    image: 'primary',
    zip: 'warning',
    other: 'default',
  };

/** 类型中文名（`title` / 无障碍文案用） */
export function fileKindLabel(kind: AttachmentFileKind): string {
  switch (kind) {
    case 'pdf':
      return 'PDF';
    case 'word':
      return 'Word';
    case 'excel':
      return 'Excel';
    case 'ppt':
      return 'PPT';
    case 'image':
      return '图片';
    case 'zip':
      return '压缩包';
    default:
      return '其他';
  }
}

/** MIME 粗分类（仅在扩展名缺失 / 不认识时兜底，避免「无扩展名的 PDF」被当成 other） */
function kindFromMime(mimeType?: string | null): AttachmentFileKind {
  const mime = (mimeType ?? '').toLowerCase();
  if (!mime) {
    return 'other';
  }
  if (mime === 'application/pdf' || mime.endsWith('/pdf')) {
    return 'pdf';
  }
  if (mime.startsWith('image/')) {
    return 'image';
  }
  if (mime.includes('spreadsheet') || mime.includes('ms-excel') || mime.includes('csv')) {
    return 'excel';
  }
  if (mime.includes('presentation') || mime.includes('ms-powerpoint')) {
    return 'ppt';
  }
  if (mime.includes('msword') || mime.includes('wordprocessing') || mime.includes('wps')) {
    return 'word';
  }
  if (mime.includes('zip') || mime.includes('rar') || mime.includes('7z') || mime.includes('tar')) {
    return 'zip';
  }
  return 'other';
}

/**
 * 判定附件类型：**扩展名优先**（与后端上传白名单同口径），扩展名不认识时用 MIME 兜底。
 *
 * @param fileName 展示用文件名（可带路径，只看最后一段的扩展名）
 * @param mimeType `sys_file.mime_type`（可空）
 */
export function fileKindOf(fileName: string, mimeType?: string | null): AttachmentFileKind {
  const ext = extensionOf(fileName);
  if (ext) {
    for (const kind of Object.keys(KIND_EXTENSIONS) as AttachmentFileKind[]) {
      if (KIND_EXTENSIONS[kind].includes(ext)) {
        return kind;
      }
    }
    // 扩展名存在但不在任何清单里（如 .ofd / .txt）：交给 MIME 兜底
    return kindFromMime(mimeType);
  }
  return kindFromMime(mimeType);
}

/** 舍入到 1 位小数并去掉多余的 `.0`（`1.5` 保留、`2.0` → `2`） */
function trimFixed(value: number): string {
  const fixed = value.toFixed(1);
  return fixed.endsWith('.0') ? fixed.slice(0, -2) : fixed;
}

/**
 * 附件大小格式化（ATT-03 卡面口径）。
 *
 * | 区间 | 显示 | 例 |
 * |------|------|----|
 * | `< 1 KB` | `N B` | 512 → `512 B` |
 * | `< 1 MB` | `N KB`（1 位小数） | 1536 → `1.5 KB` |
 * | `≥ 1 MB` | `N MB`（1 位小数） | 1536000 → `1.5 MB` |
 *
 * 边界处理：
 *
 * - `null` / `undefined` / 负数 / 非有限数 → `-`（与全局 `formatFileSize` 同口径，不显示 `NaN B`）；
 * - `1048575`（差 1 字节到 1 MB）四舍五入后会写成 `1024 KB`，这里**进到 MB** 显示 `1 MB`
 *   —— 差 1 字节却比 1 MB 少一位量级的写法会让人以为文件小了一千倍；
 * - 附件单文件上限是 100 MB（storage 策略），所以最大单位到 MB 就够，不再往 GB 长。
 */
export function formatAttachmentSize(size?: number | null): string {
  if (size === undefined || size === null || !Number.isFinite(size) || size < 0) {
    return '-';
  }
  const bytes = Math.round(size);
  if (bytes < 1024) {
    return `${bytes} B`;
  }
  const kb = bytes / 1024;
  if (kb < 1024) {
    const rounded = Number(trimFixed(kb));
    // 四舍五入到 1 位后可能刚好等于 1024（1048575 字节）→ 进 MB
    if (rounded < 1024) {
      return `${trimFixed(kb)} KB`;
    }
  }
  return `${trimFixed(bytes / (1024 * 1024))} MB`;
}

/**
 * 把 `from` 位置的元素移到 `to`（返回**新数组**，不改入参）。
 *
 * 手动排序（上移 / 下移）只需相邻交换，但这里做成通用的「移动」：
 * 越界索引原样返回副本，调用方（上一行 / 下一行的按钮）不必自己判边界。
 */
export function moveWithin<T>(list: readonly T[], from: number, to: number): T[] {
  const copy = [...list];
  if (!Number.isInteger(from) || !Number.isInteger(to)) {
    return copy;
  }
  if (from < 0 || from >= copy.length || to < 0 || to >= copy.length || from === to) {
    return copy;
  }
  const [moved] = copy.splice(from, 1);
  copy.splice(to, 0, moved);
  return copy;
}

/**
 * 面板里一行的「派生展示信息」（图标 / 类型名 / 大小文案）。
 *
 * 放在纯逻辑层是为了让单测能钉住「同一份数据 → 同样的图标和大小」，
 * 而不是只能靠截图看。
 */
export interface AttachmentDisplay {
  /** 文件类型 */
  kind: AttachmentFileKind;
  /** TDesign 图标名 */
  icon: string;
  /** 图标主题色（WPS 三件套固定 `danger` 红） */
  theme: 'danger' | 'primary' | 'success' | 'warning' | 'default';
  /** 类型中文名（WPS 文件显示「WPS 文字 / 表格 / 演示」，其余按 kind） */
  kindLabel: string;
  /** 大小文案（B / KB / MB） */
  sizeLabel: string;
  /** 能否在线预览（ATT-05：只有图片 / PDF 给「预览」按钮，Office 一律只下载） */
  previewable: boolean;
}

/**
 * 由 (文件名, MIME, 字节数) 算出展示信息（单一入口，面板不各写一份）。
 *
 * # 配色口径（ATT-05 用户拍板）
 *
 * | 家族 | 扩展名 | 图标 | 颜色 |
 * |------|--------|------|------|
 * | Word | doc / docx | `file-word` | 蓝（`primary`） |
 * | Excel | xls / xlsx / csv | `file-excel` | 绿（`success`） |
 * | PPT | ppt / pptx | `file-powerpoint` | 橙（`warning`） |
 * | WPS | wps / et / dps | 同对应形状 | **红（`danger`）** |
 *
 * # 「预览」按钮的判据也在这里
 *
 * `previewable` 直接给面板用（`v-if`），面板不许自己再判一次扩展名 ——
 * 「列表说能预览、弹窗说不能预览」这种自相矛盾只可能来自两处判据。
 */
export function describeAttachment(
  fileName: string,
  mimeType: string | null | undefined,
  fileSize: number,
): AttachmentDisplay {
  const kind = fileKindOf(fileName, mimeType);
  const wpsLabel = wpsKindLabelOf(fileName);
  return {
    kind,
    icon: KIND_ICONS[kind],
    theme: wpsLabel ? 'danger' : KIND_THEMES[kind],
    kindLabel: wpsLabel ?? fileKindLabel(kind),
    sizeLabel: formatAttachmentSize(fileSize),
    previewable: isPreviewable(resolvePreviewKind(fileName, mimeType)),
  };
}
