/**
 * 极简 Markdown → HTML（**只用于管理端「编辑预览」**）。
 *
 * # 为什么需要它
 *
 * 公告有后端权威预览接口（`POST /business/announcement/preview`，走 `Recipe::Announcement`），
 * 但帮助文档**没有**预览接口（只能保存后再看）。而批次 C 的红线是「不引入新前端依赖」，
 * 没有 `marked` / `markdown-it` 可用，所以这里写一个 120 行的子集渲染器：
 * 标题 / 段落 / 粗斜体 / 行内代码 / 链接 / 图片 / 无序有序列表 / 引用 / 分隔线 / 围栏代码块。
 *
 * # 两条纪律
 *
 * 1. **输入一律先转义**：所有文本节点都过 {@link escapeHtml}，绝不把原文拼进标签属性；
 * 2. **输出仍要净化**：本函数的结果交给 `MarkdownViewer`，那里会再过一遍
 *    `sanitizeHtml`（白名单），即便这里漏了什么，也不会执行脚本。
 *
 * # 与后端渲染的差异（预览有差异是预期内的）
 *
 * 不支持表格 / 脚注 / 定义列表 / HTML 内嵌块；heading 锚点用 `p-{level}-{index}`，
 * 与后端 `h-{level}-{index}` 不同 —— 预览不参与 TOC 跳转，正式渲染以后端为准。
 */
import { escapeHtml } from '@/utils/sanitize-html';

/** 代码块占位标记（先抽出来，避免块内 `#` `*` 被当语法处理） */
const CODE_PLACEHOLDER = '\u0000CODE%d\u0000';

/** 行内格式：行内代码 → 粗体 → 斜体 → 链接 → 图片（顺序不能反，否则 `**` 会吃掉链接） */
function renderInline(text: string): string {
  let out = escapeHtml(text);
  // 行内代码先处理，且保护其内容不再被后续规则改写
  const codes: string[] = [];
  out = out.replace(/`([^`]+)`/g, (_match, code: string) => {
    codes.push(code);
    return `\u0001${codes.length - 1}\u0001`;
  });
  out = out
    .replace(/!\[([^\]]*)\]\(([^)\s]+)\)/g, (_m, alt: string, src: string) => `<img src="${src}" alt="${alt}" />`)
    .replace(/\[([^\]]+)\]\(([^)\s]+)\)/g, (_m, label: string, href: string) => `<a href="${href}">${label}</a>`)
    .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
    .replace(/(^|[^*])\*([^*\n]+)\*/g, '$1<em>$2</em>')
    .replace(/~~([^~]+)~~/g, '<del>$1</del>');
  out = out.replace(/\u0001(\d+)\u0001/g, (_m, index: string) => `<code>${codes[Number(index)]}</code>`);
  return out;
}

/**
 * 把 Markdown 子集渲染成 HTML。
 *
 * # 参数
 * - `markdown`：Markdown 原文（可空）。
 *
 * # 返回值
 * 已转义、可读的 HTML 片段（仍建议再过 `sanitizeHtml`）。
 */
export function renderMarkdownLite(markdown: string | null | undefined): string {
  const source = (markdown ?? '').replace(/\r\n/g, '\n');
  if (!source.trim()) {
    return '';
  }

  // 1) 抽出围栏代码块
  const blocks: string[] = [];
  const withoutCode = source.replace(/```([\w+-]*)\n([\s\S]*?)```/g, (_m, lang: string, code: string) => {
    blocks.push(`<pre data-lang="${escapeHtml(lang || 'text')}"><code>${escapeHtml(code.replace(/\n$/, ''))}</code></pre>`);
    return CODE_PLACEHOLDER.replace('%d', String(blocks.length - 1));
  });

  const lines = withoutCode.split('\n');
  const out: string[] = [];
  let paragraph: string[] = [];
  let listType: 'ul' | 'ol' | null = null;
  let inQuote = false;
  let headingIndex = 0;

  const flushParagraph = (): void => {
    if (paragraph.length > 0) {
      out.push(`<p>${renderInline(paragraph.join(' '))}</p>`);
      paragraph = [];
    }
  };
  const closeList = (): void => {
    if (listType) {
      out.push(`</${listType}>`);
      listType = null;
    }
  };
  const closeQuote = (): void => {
    if (inQuote) {
      out.push('</blockquote>');
      inQuote = false;
    }
  };

  for (const line of lines) {
    const trimmed = line.trim();

    // 代码块占位
    if (/^\u0000CODE\d+\u0000$/.test(trimmed)) {
      flushParagraph();
      closeList();
      closeQuote();
      out.push(blocks[Number(trimmed.replace(/[^\d]/g, ''))]);
      continue;
    }
    // 空行
    if (!trimmed) {
      flushParagraph();
      closeList();
      closeQuote();
      continue;
    }
    // 标题
    const heading = /^(#{1,6})\s+(.*)$/.exec(trimmed);
    if (heading) {
      flushParagraph();
      closeList();
      closeQuote();
      const level = Math.min(heading[1].length, 6);
      headingIndex += 1;
      out.push(`<h${level} id="p-${level}-${headingIndex}">${renderInline(heading[2])}</h${level}>`);
      continue;
    }
    // 分隔线
    if (/^(-{3,}|\*{3,}|_{3,})$/.test(trimmed)) {
      flushParagraph();
      closeList();
      closeQuote();
      out.push('<hr />');
      continue;
    }
    // 引用
    if (trimmed.startsWith('>')) {
      flushParagraph();
      closeList();
      if (!inQuote) {
        out.push('<blockquote>');
        inQuote = true;
      }
      out.push(`<p>${renderInline(trimmed.replace(/^>\s?/, ''))}</p>`);
      continue;
    }
    closeQuote();
    // 无序列表
    const bullet = /^[-*+]\s+(.*)$/.exec(trimmed);
    if (bullet) {
      flushParagraph();
      if (listType !== 'ul') {
        closeList();
        out.push('<ul>');
        listType = 'ul';
      }
      out.push(`<li>${renderInline(bullet[1])}</li>`);
      continue;
    }
    // 有序列表
    const ordered = /^(\d+)[.)]\s+(.*)$/.exec(trimmed);
    if (ordered) {
      flushParagraph();
      if (listType !== 'ol') {
        closeList();
        out.push('<ol>');
        listType = 'ol';
      }
      out.push(`<li>${renderInline(ordered[2])}</li>`);
      continue;
    }
    // 普通段落
    closeList();
    paragraph.push(trimmed);
  }
  flushParagraph();
  closeList();
  closeQuote();
  return out.join('\n');
}

/**
 * 从 Markdown 原文提取 TOC（仅预览用；正式 TOC 一律用后端的 `toc` 字段）。
 *
 * # 参数
 * - `markdown`：Markdown 原文。
 */
export function extractTocFromMarkdown(markdown: string | null | undefined): { level: number; text: string; id: string }[] {
  const lines = (markdown ?? '').replace(/\r\n/g, '\n').split('\n');
  const items: { level: number; text: string; id: string }[] = [];
  let inCode = false;
  let index = 0;
  for (const line of lines) {
    if (line.trim().startsWith('```')) {
      inCode = !inCode;
      continue;
    }
    if (inCode) {
      continue;
    }
    const heading = /^(#{1,6})\s+(.*)$/.exec(line.trim());
    if (!heading) {
      continue;
    }
    const level = Math.min(heading[1].length, 6);
    index += 1;
    items.push({ level, text: heading[2].trim(), id: `p-${level}-${index}` });
  }
  return items;
}
