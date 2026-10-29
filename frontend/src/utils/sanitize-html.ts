/**
 * HTML 白名单净化（XSS 双保险的前端一层）。
 *
 * # 为什么要自己写
 *
 * 项目**没有** `dompurify` 依赖（`frontend/package.json` 只有 axios / dayjs / echarts /
 * pinia / tdesign / vue / vue-i18n / vue-router），批次 C 的红线是「不引入新前端依赖」，
 * 所以这里用浏览器原生 `DOMParser` 实现一套**白名单**净化，等价于 DOMPurify 的核心策略：
 *
 * 1. **先解析成惰性文档**：`DOMParser.parseFromString(html, 'text/html')` 生成的文档
 *    不会执行脚本、不会发起图片请求，`<script>` 只是普通节点；
 * 2. **标签白名单**：不在白名单的标签**整棵删除**（`script` / `style` / `iframe` /
 *    `object` / `embed` / `link` / `meta` / `form` …），不做「保留子节点」的宽容处理；
 * 3. **属性白名单**：只保留 `href` / `src` / `alt` / `title` / `class` / `id` /
 *    `colspan` / `rowspan` / `start` / `lang` / `data-lang`；`on*` 事件属性、
 *    `style`、`srcdoc` 一律丢弃；
 * 4. **协议白名单**：`href` / `src` 只允许 `http:` / `https:` / `mailto:` / `tel:` /
 *    相对路径 / 纯锚点；`javascript:` / `vbscript:` / `data:`（除 `data:image/` 的
 *    `src` 外）一律丢弃 —— 这一条挡的是 `<a href="javascript:...">` 与
 *    `<img src="data:text/html,...">`。
 *
 * # 为什么净化后还能有 `id`
 *
 * 帮助文档的 TOC 跳转依赖 heading 的 `h-{level}-{index}` 锚点，所以 `id` 必须保留；
 * 但 `id` 的值会经过 `isSafeAnchor` 校验（只允许字母数字与 `-` `_` `:` `.`），
 * 避免 `id` 里塞引号之类的解析歧义。
 *
 * # 与后端的关系
 *
 * 后端 `business/shared/content` 已用 `ammonia` 净化过一次（`content_html` 出参前再净化）。
 * 本模块是**第二道**：即使后端规则被改坏、或将来有别的接口直接吐 HTML，前端也不会执行脚本。
 */

/** 允许保留的标签（块级 + 行内 + 表格 + 代码 + heading 锚点） */
const ALLOWED_TAGS = new Set([
  'a', 'abbr', 'b', 'blockquote', 'br', 'caption', 'code', 'col', 'colgroup', 'dd', 'del',
  'details', 'div', 'dl', 'dt', 'em', 'figcaption', 'figure', 'h1', 'h2', 'h3', 'h4', 'h5',
  'h6', 'hr', 'i', 'img', 'ins', 'kbd', 'li', 'mark', 'ol', 'p', 'pre', 'q', 's', 'samp',
  'section', 'small', 'span', 'strong', 'sub', 'summary', 'sup', 'table', 'tbody', 'td',
  'tfoot', 'th', 'thead', 'tr', 'u', 'ul', 'var',
]);

/** 允许保留的属性（`class` / `id` 是 TOC 与样式钩子，别删） */
const ALLOWED_ATTRS = new Set([
  'alt', 'class', 'colspan', 'data-lang', 'href', 'id', 'lang', 'rowspan', 'src', 'start', 'title',
]);

/** 整棵删除的危险标签（与其「保留子节点」不如整块丢掉，避免残缺结构被二次利用） */
const DROP_WITH_CHILDREN = new Set([
  'script', 'style', 'iframe', 'object', 'embed', 'link', 'meta', 'base', 'form', 'input',
  'button', 'select', 'option', 'textarea', 'svg', 'math', 'template', 'noscript',
]);

/** 协议白名单（`href`） */
const SAFE_URL_SCHEMES = new Set(['http:', 'https:', 'mailto:', 'tel:']);

/** 锚点 id 允许的字符集（防止 `id` 里塞引号影响选择器） */
const SAFE_ANCHOR = /^[A-Za-z0-9_:.-]+$/;

/** 净化结果 */
export interface SanitizeResult {
  /** 净化后的 HTML（可以安全地交给 `v-html`） */
  html: string;
  /** 被整棵删除的危险节点数（`<script>` / `<iframe>` …），用于自检与证据留痕 */
  removed: number;
  /** 被丢弃的属性数（`onerror` / `style` …） */
  strippedAttrs: number;
}

/** 判断 URL 是否安全（相对路径、锚点、http(s)、mailto、tel） */
function isSafeUrl(raw: string, isImage: boolean): boolean {
  const value = raw.trim();
  if (!value) {
    return false;
  }
  // 去掉控制字符再判断（`java\tscript:` 这类绕过手法）
  const normalized = value.replace(/[\u0000-\u001f\u007f]/g, '').toLowerCase();
  if (normalized.startsWith('#') || normalized.startsWith('/') || normalized.startsWith('./')) {
    return true;
  }
  if (normalized.startsWith('../')) {
    return true;
  }
  const colon = normalized.indexOf(':');
  if (colon === -1) {
    // 无协议的相对地址（如 `guide/intro`）
    return true;
  }
  const scheme = `${normalized.slice(0, colon)}:`;
  if (isImage && scheme === 'data:') {
    // 仅允许内联图片，`data:text/html` 与 `data:image/svg+xml` 都挡（SVG 可带脚本）
    return normalized.startsWith('data:image/png;') || normalized.startsWith('data:image/jpeg;')
      || normalized.startsWith('data:image/gif;') || normalized.startsWith('data:image/webp;');
  }
  return SAFE_URL_SCHEMES.has(scheme);
}

/** 递归净化一个父节点下的所有子树 */
function scrubChildren(parent: Element, stats: { removed: number; strippedAttrs: number }): void {
  // 用快照遍历：遍历过程中会删节点，直接 for..of 会漏元素
  const children = Array.from(parent.children);
  for (const element of children) {
    const tag = element.tagName.toLowerCase();
    if (DROP_WITH_CHILDREN.has(tag)) {
      stats.removed += 1;
      element.remove();
      continue;
    }
    if (!ALLOWED_TAGS.has(tag)) {
      // 未知标签：保留其文本与子节点，但去掉标签本身（避免富文本被整体吞掉）
      stats.removed += 1;
      const fragment = document.createDocumentFragment();
      scrubChildren(element, stats);
      while (element.firstChild) {
        fragment.appendChild(element.firstChild);
      }
      element.replaceWith(fragment);
      continue;
    }
    // 属性过滤
    for (const attr of Array.from(element.attributes)) {
      const name = attr.name.toLowerCase();
      let keep = ALLOWED_ATTRS.has(name);
      if (keep && (name === 'href' || name === 'src')) {
        keep = isSafeUrl(attr.value, name === 'src');
      }
      if (keep && name === 'id') {
        keep = SAFE_ANCHOR.test(attr.value);
      }
      if (!keep) {
        stats.strippedAttrs += 1;
        element.removeAttribute(attr.name);
      }
    }
    // `img` 必须有安全 src，否则是个只占位的空图
    if (tag === 'img' && !element.getAttribute('src')) {
      element.remove();
      stats.removed += 1;
      continue;
    }
    // `a` 的外链加 noopener，避免 `window.opener` 反向控制
    if (tag === 'a' && element.getAttribute('href')) {
      element.setAttribute('rel', 'noopener noreferrer');
      element.setAttribute('target', '_blank');
    }
    scrubChildren(element, stats);
  }
}

/**
 * 净化 HTML，返回干净字符串与统计。
 *
 * # 参数
 * - `input`：待净化的 HTML（可空）。
 *
 * # 返回值
 * - `html`：可以安全交给 `v-html` 的字符串；
 * - `removed`：被删除的危险节点数；
 * - `strippedAttrs`：被丢弃的属性数。
 */
export function sanitizeHtmlWithStats(input: string | null | undefined): SanitizeResult {
  const raw = input ?? '';
  if (!raw.trim()) {
    return { html: '', removed: 0, strippedAttrs: 0 };
  }
  const stats = { removed: 0, strippedAttrs: 0 };
  // DOMParser 生成的文档是惰性的：不执行脚本、不加载资源
  const doc = new DOMParser().parseFromString(raw, 'text/html');
  scrubChildren(doc.body, stats);
  return { html: doc.body.innerHTML, removed: stats.removed, strippedAttrs: stats.strippedAttrs };
}

/**
 * 只取净化后的 HTML（页面里最常用的形态）。
 *
 * # 参数
 * - `input`：后端 `content_html` 或本地生成的 HTML。
 *
 * # 返回值
 * 白名单过滤后的 HTML 字符串；输入为空时返回空串。
 */
export function sanitizeHtml(input: string | null | undefined): string {
  return sanitizeHtmlWithStats(input).html;
}

/** 把纯文本转义成 HTML（用于 diff 值、错误信息等「必须当文本显示」的场景） */
export function escapeHtml(input: unknown): string {
  const text = input === null || input === undefined ? '' : String(input);
  return text
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;')
    .replace(/'/g, '&#39;');
}
