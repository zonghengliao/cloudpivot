/**
 * `FlowCanvas.vue` SVG 填充约束单测 —— P15 回归防呆（连线黑块）。
 *
 * 运行：`pnpm test:unit`。
 *
 * # 为什么要有这组测试
 *
 * SVG `<path>` 的**默认 `fill` 是黑色**。P12 把连线从直角折线（`M..L..L..`）改成圆角折线
 * （`M..L..Q..L..`）后，那条 14px 宽的「透明命中层」`.flow-edge__hit` 没有 `fill: none`，
 * 二次贝塞尔围出的弧形区域被填成黑色 → 生产上每根连线都长出一块黑楔形（Safari 更明显）。
 *
 * 这类缺陷 vue-tsc 看不见、单测里的几何断言也看不见，只能在**源码契约层**钉住：
 * 「用作线条的 path 必须 fill:none / 用作图形的 path 必须显式 fill」。
 *
 * 本文件直接 `readFileSync` 读 SFC（不是运行时渲染），所以断言的是「源码里有没有这条约束」，
 * 以后谁把 `fill: none` 删了/换写法，这里先红。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
/** 被测文件（相对本测试文件定位，不受 cwd 影响） */
const SFC = resolve(HERE, '../../src/views/business/flow/components/FlowCanvas.vue');
const src = readFileSync(SFC, 'utf-8');

/** 取某个 Less 嵌套块（`&__xxx { ... }`）的完整文本；取不到返回空串 */
function lessBlock(name: string): string {
  // 从 `&__name` 到第一个「缩进回退 + }」为止：块内的属性都在更深缩进里
  const start = src.indexOf(`&__${name}`);
  if (start < 0) return '';
  const open = src.indexOf('{', start);
  if (open < 0) return '';
  // 逐字符配平花括号（块里可能有嵌套 &--modifier / &:hover）
  let depth = 0;
  for (let i = open; i < src.length; i += 1) {
    if (src[i] === '{') depth += 1;
    else if (src[i] === '}') {
      depth -= 1;
      if (depth === 0) return src.slice(start, i + 1);
    }
  }
  return '';
}

/** 取所有 `<path ... />` 开标签原文 */
function pathTags(): string[] {
  return (src.match(/<path\b[^>]*?\/?>/g) ?? []).map((s) => s.replace(/\s+/g, ' '));
}

/** 某个 path 开标签上是否自带 `fill` 属性 */
function hasFillAttr(tag: string): boolean {
  return /\bfill\s*=/.test(tag);
}

/** 取 path 开标签上的 class 值 */
function classOf(tag: string): string {
  return tag.match(/class="([^"]*)"/)?.[1] ?? '';
}

// ==================== ① 核心回归：命中层不能有黑色填充 ====================

test('① `.flow-edge__hit` 必须 `fill: none`（P15 黑块根因）', () => {
  const block = lessBlock('hit');
  assert.ok(block.includes('pointer-events: stroke'), `没找到 .flow-edge__hit 样式块：${block}`);
  assert.match(block, /fill:\s*none/, `.flow-edge__hit 缺 fill: none → 连线会被填成黑块：${block}`);
});

test('①-b 命中层的 `<path>` 也必须带 `fill="none"` 属性（样式若被 scoped 改名仍有兜底）', () => {
  const tag = pathTags().find((t) => classOf(t).includes('flow-edge__hit'));
  assert.ok(tag, '模板里找不到 .flow-edge__hit 的 <path>');
  assert.ok(hasFillAttr(tag!), `命中层 path 缺 fill 属性：${tag}`);
  assert.match(tag!, /fill\s*=\s*"none"/, `命中层 path 的 fill 必须是 none：${tag}`);
});

test('①-c 命中层仍是「只吃描边」的热区：pointer-events: stroke + 14px 描边（点击热区不能坏）', () => {
  const block = lessBlock('hit');
  assert.match(block, /pointer-events:\s*stroke/, `热区命中方式被改了：${block}`);
  assert.match(block, /stroke-width:\s*14/, `热区宽度被改了（点线会点不中）：${block}`);
  assert.match(block, /stroke:\s*transparent/, `热区描边必须透明：${block}`);
});

// ==================== ② 线层 ====================

test('② `.flow-edge__line` 必须 `fill: none`（CSS + 模板属性双写）', () => {
  const block = lessBlock('line');
  assert.match(block, /fill:\s*none/, `线层 CSS 缺 fill: none：${block}`);
  assert.match(block, /pointer-events:\s*none/, `线层不该吃指针事件（命中交给热区）：${block}`);

  const tag = pathTags().find((t) => classOf(t) === 'flow-edge__line');
  assert.ok(tag, '模板里找不到 .flow-edge__line 的 <path>');
  assert.match(tag!, /fill\s*=\s*"none"/, `线层 path 缺 fill="none"：${tag}`);
});

// ==================== ③ 箭头 marker：是填充图形，必须有明确 fill ====================

test('③ 箭头 marker 里的 path 是实心三角（`d` 以 z 闭合），必须由 `.flow-canvas__arrow` 给 fill', () => {
  const markers = src.match(/<marker[\s\S]*?<\/marker>/g) ?? [];
  assert.equal(markers.length, 2, `箭头 marker 应为 2 个（普通 / 已走过），实际 ${markers.length}`);
  for (const m of markers) {
    const tag = (m.match(/<path\b[^>]*?\/?>/) ?? [''])[0];
    assert.ok(tag, `marker 里没有 <path>：${m}`);
    assert.match(classOf(tag), /flow-canvas__arrow/, `marker path 缺 arrow 类名：${tag}`);
    // 三角形闭合路径 → 只能是填充图形（不能 fill:none，否则箭头消失）
    assert.match(tag.replace(/\s+/g, ' '), /d="[^"]*z"/i, `箭头 path 应该是闭合三角：${tag}`);
    assert.doesNotMatch(tag, /fill\s*=\s*"none"/, `箭头不能 fill="none"（会看不见）：${tag}`);
  }

  const arrow = lessBlock('arrow');
  assert.match(arrow, /fill:\s*var\(--td-/, `.flow-canvas__arrow 必须有颜色 fill：${arrow}`);
  assert.match(arrow, /&--taken\s*\{[\s\S]*?fill:\s*var\(--td-/, `已走过的箭头要有独立颜色：${arrow}`);
});

// ==================== ④ 点阵背景：不是 path，但同样不能留默认黑 ====================

test('④ 网格用 pattern（circle + rect），没有任何裸 `<path>` 网格线', () => {
  assert.match(src, /<pattern[\s\S]*?id="flow-grid-pattern"[\s\S]*?<\/pattern>/, '找不到 flow-grid-pattern');
  const pattern = src.match(/<pattern[\s\S]*?<\/pattern>/)?.[0] ?? '';
  assert.doesNotMatch(pattern, /<path\b/, '点阵背景里不该出现 <path>（会吃到默认黑色 fill）');
  assert.match(pattern, /<circle[^>]*class="flow-canvas__grid-dot"/, '点阵应该用 circle 画点');
  assert.match(lessBlock('grid-dot'), /fill:\s*color-mix|fill:\s*var\(--td-/, '网点必须有颜色 fill');
  // 挂 pattern 的 rect 显式给 fill（不是默认黑）
  assert.match(src, /class="flow-canvas__grid"[^>]*fill="url\(#flow-grid-pattern\)"/, '网格 rect 缺 fill=url(#flow-grid-pattern)');
  assert.match(lessBlock('grid'), /fill:\s*transparent/, '网格 rect 自身的 CSS fill 必须是 transparent');
});

// ==================== ⑤ 通用哨兵：新增 path 必须交代 fill 来源 ====================

test('⑤ 每个 `<path>` 都要么带 fill 属性，要么属于「实心图形」白名单（新增 path 会被这条拦住）', () => {
  // 白名单：靠 CSS `.flow-canvas__arrow { fill: … }` 拿颜色的实心三角（marker 里两个）
  const CSS_FILLED_GLYPHS = new Set([
    'flow-canvas__arrow',
    'flow-canvas__arrow flow-canvas__arrow--taken',
  ]);

  const offenders = pathTags()
    .filter((tag) => !hasFillAttr(tag) && !CSS_FILLED_GLYPHS.has(classOf(tag)))
    .map((tag) => tag.slice(0, 120));

  assert.deepEqual(
    offenders,
    [],
    `这些 <path> 既没有 fill 属性、也不在实心图形白名单里（SVG 默认 fill 是黑色）：\n${offenders.join('\n')}`,
  );
});

test('⑥ 拖拽中的虚线 path 也显式 fill="none"（同类问题，提前钉住）', () => {
  const tag = pathTags().find((t) => classOf(t) === 'flow-canvas__draft');
  assert.ok(tag, '模板里找不到 .flow-canvas__draft 的 <path>');
  assert.match(tag!, /fill\s*=\s*"none"/, `拖拽虚线缺 fill="none"：${tag}`);
});
