<template>
  <div class="mdv" :class="{ 'mdv--with-toc': showToc && tocItems.length > 0 }">
    <!-- 左：正文 -->
    <div ref="scrollRef" class="mdv__scroll" :style="{ maxHeight: maxHeight || undefined }" @scroll.passive="onScroll">
      <t-loading :loading="loading" size="small">
        <div v-if="safeHtml" ref="bodyRef" class="mdv__body" v-html="safeHtml" />
        <div v-else-if="!loading" class="mdv__empty">
          <t-icon class="mdv__empty-icon" name="file-1" />
          <p class="mdv__empty-title">{{ emptyText }}</p>
        </div>
      </t-loading>
    </div>

    <!-- 右：目录（点击滚动到对应 heading，滚动时高亮当前小节） -->
    <aside v-if="showToc && tocItems.length > 0" class="mdv__toc">
      <div class="mdv__toc-head">
        <t-icon name="view-list" />
        <span>目录</span>
        <span class="mdv__toc-count">{{ tocItems.length }}</span>
      </div>
      <ul class="mdv__toc-list">
        <li
          v-for="item in flatToc"
          :key="item.id"
          class="mdv__toc-item"
          :class="[`mdv__toc-item--l${item.level}`, { 'mdv__toc-item--active': activeId === item.id }]"
          :title="item.text"
          @click="scrollToHeading(item.id)"
        >
          {{ item.text }}
        </li>
      </ul>
    </aside>
  </div>
</template>

<script setup lang="ts">
/**
 * Markdown 渲染器（公告正文 / 帮助文档正文共用）。
 *
 * # 三件事
 *
 * 1. **XSS**：入参是后端 `content_html`（后端 `ammonia` 已净化），这里再过一遍
 *    {@link sanitizeHtml}（白名单 + 协议校验），**只有净化结果**才交给 `v-html`。
 *    组件不接收 Markdown 原文，避免「前端自造渲染器与后端不一致」这类事故。
 * 2. **TOC**：优先用后端给的 `toc`（`h-{level}-{index}` 锚点），没有就从 HTML 的
 *    h1–h4 里推导；点击平滑滚动、滚动时高亮当前小节。
 * 3. **代码块**：不引第三方高亮库（红线：不加依赖），用原生 tokenizer 给
 *    `pre > code` 上色，并为每块加「语言标签 + 复制」。
 */
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import { sanitizeHtmlWithStats } from '@/utils/sanitize-html';

/** 目录节点（与后端 `HelpTocRes` 同形，`children` 可省） */
export interface MarkdownTocNode {
  /** 标题级别 1–4 */
  level: number;
  /** 标题文本 */
  text: string;
  /** 锚点 id */
  id: string;
  /** 子节点 */
  children?: MarkdownTocNode[];
}

const props = withDefaults(
  defineProps<{
    /** 后端渲染并净化的 HTML（`content_html`） */
    html?: string;
    /** 后端生成的目录树（可空，空则从 HTML 推导） */
    toc?: MarkdownTocNode[];
    /** 是否显示右侧目录 */
    showToc?: boolean;
    /** 加载态 */
    loading?: boolean;
    /** 正文为空时的提示 */
    emptyText?: string;
    /** 滚动区最大高度（如 `'70vh'`；不传则不限高，由父容器滚动） */
    maxHeight?: string;
  }>(),
  { html: '', toc: () => [], showToc: true, loading: false, emptyText: '暂无内容', maxHeight: '' },
);

const scrollRef = ref<HTMLElement | null>(null);
const bodyRef = ref<HTMLElement | null>(null);
const activeId = ref('');

/** 净化结果（含被删除的危险节点数，供自检 / 证据使用） */
const sanitized = computed(() => sanitizeHtmlWithStats(props.html));

/** 交给 `v-html` 的唯一字符串 */
const safeHtml = computed(() => sanitized.value.html);

/** 从净化后的 HTML 推导目录（后端没给 toc 时的兜底） */
const derivedToc = ref<MarkdownTocNode[]>([]);

/** 实际使用的目录：后端优先 */
const tocItems = computed<MarkdownTocNode[]>(() => (props.toc.length > 0 ? props.toc : derivedToc.value));

/** 拍平目录（渲染成单列，用缩进表达层级；比嵌套 ul 更省 DOM） */
const flatToc = computed<{ id: string; text: string; level: number }[]>(() => {
  const out: { id: string; text: string; level: number }[] = [];
  const walk = (nodes: MarkdownTocNode[]): void => {
    for (const node of nodes) {
      out.push({ id: node.id, text: node.text, level: Math.min(Math.max(node.level, 1), 4) });
      if (node.children?.length) {
        walk(node.children);
      }
    }
  };
  walk(tocItems.value);
  return out;
});

/** 暴露给父组件 / 自检脚本：净化后的 HTML 与被删除的危险节点数 */
defineExpose({
  sanitizedHtml: safeHtml,
  removedNodes: computed(() => sanitized.value.removed),
  strippedAttrs: computed(() => sanitized.value.strippedAttrs),
});

/** 高亮用的关键字（各语言通用的一小撮，够了；不做完整语法树） */
const KEYWORDS = new Set([
  'async', 'await', 'break', 'case', 'catch', 'class', 'const', 'continue', 'default', 'delete',
  'do', 'else', 'enum', 'export', 'extends', 'false', 'finally', 'fn', 'for', 'from', 'function',
  'if', 'impl', 'import', 'in', 'instanceof', 'interface', 'let', 'loop', 'match', 'mod', 'mut',
  'new', 'null', 'of', 'package', 'pub', 'return', 'self', 'static', 'struct', 'super', 'switch',
  'this', 'throw', 'trait', 'true', 'try', 'type', 'typeof', 'undefined', 'use', 'var', 'void',
  'where', 'while', 'yield',
]);

/** 代码块高亮：把 textContent 重新切词，用 span 上色（全程不拼未转义字符串） */
function highlightCodeBlock(code: HTMLElement): void {
  const source = code.textContent ?? '';
  if (!source.trim()) {
    return;
  }
  const fragment = document.createDocumentFragment();
  // 依次匹配：注释 / 字符串 / 数字 / 标识符，未命中的片段按原样输出
  const tokenPattern = /(\/\/[^\n]*|\/\*[\s\S]*?\*\/|#[^\n]*|"[^"\n]*"|'[^'\n]*'|`[^`]*`|\b\d+(?:\.\d+)?\b|[A-Za-z_$][\w$]*)/g;
  let lastIndex = 0;
  let match: RegExpExecArray | null = tokenPattern.exec(source);
  while (match) {
    if (match.index > lastIndex) {
      fragment.appendChild(document.createTextNode(source.slice(lastIndex, match.index)));
    }
    const token = match[0];
    const span = document.createElement('span');
    if (token.startsWith('//') || token.startsWith('/*') || token.startsWith('#')) {
      span.className = 'tok-comment';
    } else if (/^["'`]/.test(token)) {
      span.className = 'tok-string';
    } else if (/^\d/.test(token)) {
      span.className = 'tok-number';
    } else if (KEYWORDS.has(token)) {
      span.className = 'tok-keyword';
    } else {
      span.className = '';
    }
    // textContent 赋值：即使 token 里有 `<` 也不会变成标签
    span.textContent = token;
    fragment.appendChild(span);
    lastIndex = match.index + token.length;
    match = tokenPattern.exec(source);
  }
  if (lastIndex < source.length) {
    fragment.appendChild(document.createTextNode(source.slice(lastIndex)));
  }
  code.textContent = '';
  code.appendChild(fragment);
}

/** 给每个代码块加语言标签与复制按钮（幂等：重复渲染不会叠加） */
function decorateCodeBlocks(): void {
  const root = bodyRef.value;
  if (!root) {
    return;
  }
  const blocks = Array.from(root.querySelectorAll('pre'));
  for (const pre of blocks) {
    if (pre.dataset['decorated'] === '1') {
      continue;
    }
    pre.dataset['decorated'] = '1';
    const code = pre.querySelector('code');
    const language = detectLanguage(code, pre);
    highlightCodeBlock((code ?? pre) as HTMLElement);
    const bar = document.createElement('div');
    bar.className = 'mdv-code-bar';
    const label = document.createElement('span');
    label.className = 'mdv-code-bar__lang';
    label.textContent = language || 'text';
    const copy = document.createElement('button');
    copy.type = 'button';
    copy.className = 'mdv-code-bar__copy';
    copy.textContent = '复制';
    copy.addEventListener('click', () => {
      void copyCode((code ?? pre).textContent ?? '', copy);
    });
    bar.appendChild(label);
    bar.appendChild(copy);
    pre.insertBefore(bar, pre.firstChild);
    pre.classList.add('mdv-pre');
  }
}

/** 从 `data-lang` / `language-x` class 里取语言名 */
function detectLanguage(code: Element | null, pre: Element): string {
  const fromCode = code?.getAttribute('data-lang') || pre.getAttribute('data-lang') || '';
  if (fromCode) {
    return fromCode;
  }
  const className = code?.className || '';
  const matched = /language-([\w+-]+)/.exec(className);
  return matched?.[1] ?? '';
}

/** 复制代码（失败时提示，不静默） */
async function copyCode(text: string, button: HTMLElement): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
    const original = button.textContent;
    button.textContent = '已复制';
    window.setTimeout(() => {
      button.textContent = original ?? '复制';
    }, 1200);
  } catch {
    void MessagePlugin.warning('复制失败，请手动选择代码');
  }
}

/** 从净化后的 HTML 推导目录（无 `toc` 入参时） */
function buildDerivedToc(): void {
  const root = bodyRef.value;
  if (!root || props.toc.length > 0) {
    derivedToc.value = [];
    return;
  }
  const headings = Array.from(root.querySelectorAll('h1, h2, h3, h4'));
  const items: MarkdownTocNode[] = [];
  headings.forEach((heading, index) => {
    const level = Number(heading.tagName.slice(1));
    let id = heading.id;
    if (!id) {
      id = `mdv-h-${level}-${index}`;
      heading.id = id;
    }
    items.push({ level, text: heading.textContent?.trim() || `第 ${index + 1} 节`, id });
  });
  derivedToc.value = items;
}

/** 点击目录：滚动到对应 heading（容器内平滑滚动，留 12px 呼吸感） */
function scrollToHeading(id: string): void {
  const heading = bodyRef.value?.querySelector<HTMLElement>(`#${cssEscape(id)}`);
  if (!heading) {
    return;
  }
  activeId.value = id;
  const container = scrollRef.value;
  if (!container) {
    heading.scrollIntoView({ behavior: 'smooth', block: 'start' });
    return;
  }
  const offset = heading.getBoundingClientRect().top - container.getBoundingClientRect().top + container.scrollTop - 12;
  container.scrollTo({ top: Math.max(offset, 0), behavior: 'smooth' });
}

/** `id` 里可能有 `:` `.` 等字符，作为选择器要转义 */
function cssEscape(id: string): string {
  if (typeof CSS !== 'undefined' && typeof CSS.escape === 'function') {
    return CSS.escape(id);
  }
  return id.replace(/([^\w-])/g, '\\$1');
}

/** 滚动时更新当前小节（rAF 节流，避免滚动卡顿） */
let scrollFrame = 0;
function onScroll(): void {
  if (scrollFrame) {
    return;
  }
  scrollFrame = window.requestAnimationFrame(() => {
    scrollFrame = 0;
    const container = scrollRef.value;
    const root = bodyRef.value;
    if (!container || !root) {
      return;
    }
    const headings = Array.from(root.querySelectorAll<HTMLElement>('h1, h2, h3, h4'));
    const threshold = container.getBoundingClientRect().top + 80;
    let current = '';
    for (const heading of headings) {
      if (heading.getBoundingClientRect().top <= threshold) {
        current = heading.id;
      } else {
        break;
      }
    }
    if (current && current !== activeId.value) {
      activeId.value = current;
    }
  });
}

/** 正文变化 → 重新推导目录 + 装饰代码块 + 复位高亮 */
function afterRender(): void {
  buildDerivedToc();
  decorateCodeBlocks();
  activeId.value = flatToc.value[0]?.id ?? '';
}

watch(safeHtml, () => {
  void nextTick(afterRender);
});

onMounted(() => {
  void nextTick(afterRender);
});

onBeforeUnmount(() => {
  if (scrollFrame) {
    window.cancelAnimationFrame(scrollFrame);
  }
});
</script>

<style scoped lang="less">
.mdv {
  display: flex;
  gap: 20px;
  min-height: 0;

  &__scroll {
    flex: 1;
    min-width: 0;
    overflow-y: auto;
    position: relative;
  }

  &__body {
    font-size: 14px;
    line-height: 1.85;
    color: var(--td-text-color-primary);
    word-break: break-word;

    :deep(h1),
    :deep(h2),
    :deep(h3),
    :deep(h4) {
      margin: 22px 0 10px;
      font-weight: 600;
      line-height: 1.4;
      color: var(--td-text-color-primary);
      scroll-margin-top: 12px;
    }

    :deep(h1) {
      font-size: 22px;
      padding-bottom: 8px;
      border-bottom: 1px solid var(--td-component-stroke);
    }

    :deep(h2) {
      font-size: 18px;
    }

    :deep(h3) {
      font-size: 16px;
    }

    :deep(h4) {
      font-size: 14px;
      color: var(--td-text-color-secondary);
    }

    :deep(p) {
      margin: 10px 0;
    }

    :deep(a) {
      color: var(--td-brand-color);
      text-decoration: none;

      &:hover {
        text-decoration: underline;
      }
    }

    :deep(ul),
    :deep(ol) {
      margin: 10px 0;
      padding-left: 22px;
    }

    :deep(li) {
      margin: 4px 0;
    }

    :deep(blockquote) {
      margin: 12px 0;
      padding: 8px 14px;
      border-left: 3px solid var(--td-brand-color-4);
      background: var(--app-accent-soft);
      color: var(--td-text-color-secondary);
      border-radius: 0 6px 6px 0;
    }

    :deep(hr) {
      margin: 20px 0;
      border: 0;
      border-top: 1px solid var(--td-component-stroke);
    }

    :deep(table) {
      width: 100%;
      margin: 14px 0;
      border-collapse: collapse;
      font-size: 13px;
    }

    :deep(th) {
      background: var(--app-table-head-bg);
      font-weight: 600;
      text-align: left;
    }

    :deep(th),
    :deep(td) {
      padding: 8px 10px;
      border: 1px solid var(--td-component-stroke);
    }

    :deep(img) {
      max-width: 100%;
      border-radius: 6px;
    }

    :deep(code) {
      padding: 1px 5px;
      border-radius: 4px;
      font-family: var(--app-font-mono, ui-monospace, SFMono-Regular, Menlo, Consolas, monospace);
      font-size: 13px;
      background: var(--td-bg-color-container-hover);
      color: var(--td-text-color-primary);
    }

    :deep(.mdv-pre) {
      position: relative;
      margin: 14px 0;
      padding: 34px 14px 14px;
      overflow-x: auto;
      border-radius: 8px;
      background: var(--td-bg-color-container-hover);
      border: 1px solid var(--td-component-stroke);

      :deep(code) {
        display: block;
        padding: 0;
        background: transparent;
        font-size: 13px;
        line-height: 1.7;
        white-space: pre;
      }
    }

    :deep(.mdv-code-bar) {
      position: absolute;
      top: 0;
      left: 0;
      right: 0;
      display: flex;
      align-items: center;
      justify-content: space-between;
      padding: 4px 10px;
      font-size: 12px;
      color: var(--td-text-color-placeholder);
      border-bottom: 1px solid var(--td-component-stroke);
    }

    :deep(.mdv-code-bar__copy) {
      padding: 0 6px;
      font-size: 12px;
      color: var(--td-brand-color);
      background: transparent;
      border: 0;
      cursor: pointer;

      &:hover {
        text-decoration: underline;
      }
    }

    :deep(.tok-keyword) {
      color: var(--td-brand-color);
      font-weight: 600;
    }

    :deep(.tok-string) {
      color: var(--td-success-color);
    }

    :deep(.tok-number) {
      color: var(--td-warning-color);
    }

    :deep(.tok-comment) {
      color: var(--td-text-color-placeholder);
      font-style: italic;
    }
  }

  &__empty {
    padding: 60px 0;
    text-align: center;
  }

  &__empty-icon {
    font-size: 44px;
    color: var(--td-text-color-placeholder);
  }

  &__empty-title {
    margin: 10px 0 0;
    font-size: 14px;
    color: var(--td-text-color-secondary);
  }

  &__toc {
    width: 208px;
    flex: 0 0 208px;
    align-self: flex-start;
    max-height: 100%;
    overflow-y: auto;
    padding-left: 12px;
    border-left: 1px solid var(--td-component-stroke);
  }

  &__toc-head {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 8px;
    font-size: 12px;
    font-weight: 600;
    color: var(--td-text-color-secondary);
  }

  &__toc-count {
    padding: 0 6px;
    border-radius: 8px;
    background: var(--td-bg-color-container-hover);
    color: var(--td-text-color-placeholder);
  }

  &__toc-list {
    margin: 0;
    padding: 0;
    list-style: none;
  }

  &__toc-item {
    padding: 4px 8px;
    font-size: 13px;
    color: var(--td-text-color-secondary);
    border-radius: 4px;
    cursor: pointer;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    transition: color 0.15s, background-color 0.15s;

    &:hover {
      color: var(--td-brand-color);
      background: var(--td-bg-color-container-hover);
    }

    &--active {
      color: var(--td-brand-color);
      background: var(--app-accent-soft);
      font-weight: 600;
    }

    &--l2 {
      padding-left: 20px;
    }

    &--l3 {
      padding-left: 32px;
    }

    &--l4 {
      padding-left: 44px;
      font-size: 12px;
    }
  }
}
</style>
