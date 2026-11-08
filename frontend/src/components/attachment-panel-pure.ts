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

import { extensionOf } from './attachment-preview-pure';

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
  /** 图标主题色 */
  theme: 'danger' | 'primary' | 'success' | 'warning' | 'default';
  /** 类型中文名 */
  kindLabel: string;
  /** 大小文案（B / KB / MB） */
  sizeLabel: string;
}

/** 由 (文件名, MIME, 字节数) 算出展示信息（单一入口，面板不各写一份） */
export function describeAttachment(
  fileName: string,
  mimeType: string | null | undefined,
  fileSize: number,
): AttachmentDisplay {
  const kind = fileKindOf(fileName, mimeType);
  return {
    kind,
    icon: KIND_ICONS[kind],
    theme: KIND_THEMES[kind],
    kindLabel: fileKindLabel(kind),
    sizeLabel: formatAttachmentSize(fileSize),
  };
}
