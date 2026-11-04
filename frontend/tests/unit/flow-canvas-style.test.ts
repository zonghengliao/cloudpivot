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

// ==================== ⑦ SAFARI-FLOW：节点坐标不许再回到 <g transform> ====================
//
// Safari（WebKit）在「<g transform="translate(x,y)"> 包 <foreignObject>」这个组合下，
// foreignObject 里的 HTML 可能不跟随外层 g 的 translate → 整片卡片塌陷到画布左上角叠在一起
// （2026-11 用户真机实测：连线 / 条件标签 / 缩略图都正常，只有卡片跑位，Chrome 正常）。
// 修法是把节点坐标放在 foreignObject 的 x/y 上，锚点 / 角标用绝对坐标。
// 这组断言钉住「结构不被改回去」，因为这种缺陷 vue-tsc 与几何单测都看不见。

test('⑦ `.flow-node` 的 <g> 上不允许再出现 transform（Safari foreignObject 塌陷的根因）', () => {
  const groupTags = (src.match(/<g\b[^>]*class="flow-node"[^>]*>/g) ?? []).map((t: string) => t.replace(/\s+/g, ' '));
  assert.equal(groupTags.length, 1, `模板里应只有一个 .flow-node 的 <g> 起始标签，实际 ${groupTags.length}`);
  const tag = groupTags[0];
  assert.doesNotMatch(tag, /:transform\s*=/, `.flow-node 的 <g> 又绑了 transform（Safari 会塌陷）：${tag}`);
  assert.doesNotMatch(tag, /(^|\s)transform\s*=/, `.flow-node 的 <g> 又写了 transform（Safari 会塌陷）：${tag}`);
});

test('⑦-b foreignObject 用 node.x / node.y 定位（不再是 x="0" y="0"）', () => {
  const foTags = (src.match(/<foreignObject\b[^>]*class="flow-node__fo"[^>]*>/g) ?? []).map((t: string) => t.replace(/\s+/g, ' '));
  assert.equal(foTags.length, 1, `模板里应只有一个 .flow-node__fo，实际 ${foTags.length}`);
  const tag = foTags[0];
  assert.match(tag, /:x="node\.x"/, `foreignObject 缺 :x="node.x"：${tag}`);
  assert.match(tag, /:y="node\.y"/, `foreignObject 缺 :y="node.y"：${tag}`);
  assert.doesNotMatch(tag, /\sx="0"/, `foreignObject 还写死 x="0"（节点会叠在原点）：${tag}`);
  assert.match(tag, /:width="node\.size\.w"/, `foreignObject 的宽度必须仍取 node.size.w：${tag}`);
  assert.match(tag, /:height="node\.size\.h"/, `foreignObject 的高度必须仍取 node.size.h：${tag}`);
});

test('⑦-c 锚点 / 校验角标用绝对坐标（跟着 node.x / node.y 偏移，而不是靠 g 的 translate）', () => {
  // 入口锚点：cx = node.x
  assert.match(src, /<circle class="flow-node__port" :cx="node\.x"/, '入口锚点必须用 :cx="node.x"');
  // 出口锚点：cx = node.x + node.size.w
  assert.match(src, /:cx="node\.x \+ node\.size\.w"/, '出口锚点必须用 :cx="node.x + node.size.w"');
  // 两者 cy 都是 node.y + node.size.h / 2
  const portCys = src.match(/:cy="node\.y \+ node\.size\.h \/ 2"/g) ?? [];
  assert.equal(portCys.length, 2, `两个锚点的 cy 都必须是 node.y + node.size.h / 2，实际命 ${portCys.length} 处`);
  // 角标：不再用 <g transform>，圆心/文字都用绝对坐标
  const issueTag = (src.match(/<g\b[^>]*class="flow-node__issue"[^>]*>/g) ?? []).map((t: string) => t.replace(/\s+/g, ' '));
  assert.equal(issueTag.length, 1, `模板里应只有一个 .flow-node__issue，实际 ${issueTag.length}`);
  assert.doesNotMatch(issueTag[0], /:transform\s*=/, `校验角标又回到 transform 定位：${issueTag[0]}`);
  assert.match(src, /<circle :cx="node\.x \+ node\.size\.w - 4" :cy="node\.y \+ 4" r="8"/, '角标圆心必须是 (node.x+w-4, node.y+4)');
  assert.match(src, /<text :x="node\.x \+ node\.size\.w - 4" :y="node\.y \+ 8">/, '角标文字必须是绝对坐标（node.x+w-4, node.y+8）');
});

// ==================== ⑧ SAFARI-FLOW 后续：平移缩放要从 <g> 挪到 svg 的 CSS transform ====================
//
// 实测（Playwright WebKit 2370，见 scripts/verify/flow_canvas_diag.cjs）：WebKit 对
// 「祖先 <g transform> + foreignObject 里的 HTML」只更新布局、不更新绘制 —— 卡片会被画在
// 未变换的位置（6 张卡全糊在画布原点，就是用户看到的样子）。
// 把 translate/scale 换成 svg 元素自己的 CSS transform 后，整个 svg 当一个普通盒子被合成，
// 两引擎的绘制位置才与布局一致。

test('⑧ scene <g> 上不许有 transform，平移缩放必须挂在 svg 的 :style 上', () => {
  const sceneTag = (src.match(/<g ref="sceneRef"[^>]*>/g) ?? []).map((t: string) => t.replace(/\s+/g, ' '));
  assert.equal(sceneTag.length, 1, `模板里应只有一个 scene <g>，实际 ${sceneTag.length}`);
  assert.doesNotMatch(sceneTag[0], /:transform\s*=/, `scene <g> 又绑了 transform（WebKit 下卡片会画错位置）：${sceneTag[0]}`);
  assert.match(src, /:style="sceneStyle"/, 'svg 元素必须绑定 :style="sceneStyle"（平移缩放的唯一承载）');
  assert.match(src, /const sceneStyle = computed\(/, '缺少 sceneStyle computed');
  assert.match(src, /transformOrigin: '0 0'/, 'sceneStyle 必须 transform-origin: 0 0（与 <g transform> 等价的前提）');
  // svg 自身要允许内容画到盒子外（缩小/平移到画布外时不能裁掉），裁剪交给容器
  assert.match(src, /&__svg \{[\s\S]*?overflow: visible;/, '.flow-canvas__svg 必须 overflow: visible');
});

test('⑨ 卡片命中交给同位置的 SVG rect（否则 WebKit 下 HTML 卡片盖住锚点，拉不了线）', () => {
  const hitTag = (src.match(/<rect\b[^>]*class="flow-node__hit"[^>]*>/g) ?? []).map((t: string) => t.replace(/\s+/g, ' '));
  assert.equal(hitTag.length, 1, `模板里应只有一个 .flow-node__hit，实际 ${hitTag.length}`);
  const tag = hitTag[0];
  assert.match(tag, /:x="node\.x"/, `命中层缺 :x="node.x"：${tag}`);
  assert.match(tag, /:y="node\.y"/, `命中层缺 :y="node.y"：${tag}`);
  assert.match(tag, /:width="node\.size\.w"/, `命中层缺宽度：${tag}`);
  assert.match(tag, /:height="node\.size\.h"/, `命中层缺高度：${tag}`);
  assert.match(tag, /fill="transparent"/, `命中层必须透明（否则卡片上会出现一个黑块）：${tag}`);
  // 卡片本身必须不吃指针事件，否则锚点仍会被 HTML 盖住
  assert.match(src, /&__card \{[\s\S]*?pointer-events: none;/, '.flow-node__card 必须 pointer-events: none');
});
