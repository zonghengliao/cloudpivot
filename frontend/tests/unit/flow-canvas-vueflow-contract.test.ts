/**
 * Vue Flow 画布与老画布的**契约对比**（VUE-FLOW-1）。
 *
 * 运行：`pnpm test:unit`。
 *
 * # 为什么要有这组测试
 *
 * 卡片的第一条验收就是「11 个 props / emits / 暴露方法一个不能少，签名与现有完全一致」，
 * 而这件事 vue-tsc **看不见** —— 两个 SFC 各自 `defineProps` 自己的形状，
 * 少了 `takenPairs` 也只是「没人传」而不是类型错误。等到线上发现进度图不亮才回头查，
 * 代价远大于这里读一遍源码。
 *
 * 所以本文件直接 `readFileSync` 两个 SFC，把 `defineProps` / `defineEmits` / `defineExpose`
 * 三处清单抽出来做**集合比较**：任何一边加/删字段，另一边没跟上就红。
 *
 * 同时钉住三条「视觉/行为等价」的源码约束：
 * 1. 节点卡片规格（11px 圆角 / 3.5px 色条 / 30×30 图标底块）两边都还在；
 * 2. Handle 必须是 `target=Left` + `source=Right`（POC 踩过的坑：不给 Handle 会兜底成
 *    bottom→top，连线从底部出、顶部进，和老画布的「左进右出」完全不同）；
 * 3. 连线仍复用 `graph.ts` 的 `edgePath` + `rounded-path.ts` 的 `roundedEdgePath`（几何唯一来源）。
 */
import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const DIR = resolve(HERE, '../../src/views/business/flow/components');

/** 老画布（纯 SVG 自绘，本卡一行不改） */
const OLD_SRC = readFileSync(resolve(DIR, 'FlowCanvas.vue'), 'utf-8');
/** 新画布（Vue Flow 基座） */
const NEW_SRC = readFileSync(resolve(DIR, 'FlowCanvasVueFlow.vue'), 'utf-8');
/** 切换器（flag 的唯一接入点） */
const SWITCH_SRC = readFileSync(resolve(DIR, 'FlowCanvasSwitch.vue'), 'utf-8');

/** 去掉块注释 / 行注释：注释里出现的 `defineProps<{ x?: boolean }>()` 会骗过 `indexOf` */
function stripComments(src: string): string {
  return src
    .replace(/\/\*[\s\S]*?\*\//g, '')
    .split('\n')
    .map((line) => line.replace(/\/\/.*$/, ''))
    .join('\n');
}

const OLD_CODE = stripComments(OLD_SRC);
const NEW_CODE = stripComments(NEW_SRC);
const SWITCH_CODE = stripComments(SWITCH_SRC);

/** 取 `defineProps<{ ... }>()` 里的顶层字段名（`name?:` / `name:`） */
function propNames(src: string): string[] {
  const start = src.indexOf('defineProps<{');
  assert.ok(start >= 0, 'defineProps<{ 没找到');
  const end = src.indexOf('}>()', start);
  assert.ok(end > start, 'defineProps 块没闭合');
  const block = src.slice(start, end);
  return [...block.matchAll(/^ {2,6}([A-Za-z_$][\w$]*)\??:/gm)].map((match) => match[1]).sort();
}

/** 取 `defineEmits<{ ... }>` 里的所有 `(event: 'xxx', ...)` 事件名 */
function emitNames(src: string): string[] {
  const start = src.indexOf('defineEmits<{');
  assert.ok(start >= 0, 'defineEmits<{ 没找到');
  const end = src.indexOf('}>()', start);
  assert.ok(end > start, 'defineEmits 块没闭合');
  const block = src.slice(start, end);
  return [...block.matchAll(/\(event: '([\w-]+)'/g)].map((match) => match[1]).sort();
}

/** 取 `defineExpose({ ... })` 里暴露的方法名 */
function exposeNames(src: string): string[] {
  const start = src.indexOf('defineExpose({');
  assert.ok(start >= 0, 'defineExpose({ 没找到');
  // 逐字符配平花括号（里面有箭头函数体，不能简单找第一个 `})`）
  let depth = 0;
  for (let i = start; i < src.length; i += 1) {
    if (src[i] === '{') depth += 1;
    else if (src[i] === '}') {
      depth -= 1;
      if (depth === 0) {
        const block = src.slice(start, i + 1);
        // 两种写法都要认：`fitView,`（简写，老画布用）与 `fitView: () => …`（转发，新画布 / 切换器用）
        const longhand = [...block.matchAll(/^ {2}([A-Za-z_$][\w$]*):/gm)].map((match) => match[1]);
        const shorthand = [...block.matchAll(/^ {2}([A-Za-z_$][\w$]*),/gm)].map((match) => match[1]);
        return [...new Set([...longhand, ...shorthand])].sort();
      }
    }
  }
  throw new Error('defineExpose 块没闭合');
}

// ==================== ① props ====================

test('① props：新画布覆盖老画布全部字段（一个都不能少）', () => {
  const oldProps = propNames(OLD_CODE);
  const newProps = propNames(NEW_CODE);
  // 老画布 13 个（卡面写 11 个，实际含 fitOnMount / canUndoLayout）
  assert.equal(oldProps.length, 13, `老画布 props 数量变了：${oldProps.join(', ')}`);
  for (const name of oldProps) {
    assert.ok(newProps.includes(name), `新画布缺 props.${name}`);
  }
});

test('② props：新画布允许的额外字段只有 autoFit 兼容别名', () => {
  const oldProps = new Set(propNames(OLD_CODE));
  const extra = propNames(NEW_CODE).filter((name) => !oldProps.has(name));
  assert.deepEqual(extra, ['autoFit'], `新画布多出了非预期 props：${extra.join(', ')}`);
});

test('③ props：切换器声明了全部字段（否则调用方传参过不了 vue-tsc）', () => {
  const switchProps = propNames(SWITCH_CODE);
  for (const name of propNames(NEW_CODE)) {
    assert.ok(switchProps.includes(name), `切换器缺 props.${name}`);
  }
});

// ==================== ② emits ====================

test('④ emits：两套画布事件名完全一致（10 个）', () => {
  const oldEmits = emitNames(OLD_CODE);
  const newEmits = emitNames(NEW_CODE);
  assert.equal(oldEmits.length, 10, `老画布事件数量变了：${oldEmits.join(', ')}`);
  assert.deepEqual(newEmits, oldEmits);
});

test('⑤ emits：切换器转发了全部事件（漏一个就是「点了没反应」）', () => {
  for (const name of emitNames(OLD_CODE)) {
    // `'select-node': fn` 与 `connect: fn` 两种写法都算转发
    const pattern = new RegExp(`(?:'${name}'|\\b${name})\\s*:`);
    assert.match(SWITCH_CODE, pattern, `切换器没转发事件 ${name}`);
  }
});

// ==================== ③ 暴露方法 ====================

test('⑥ expose：新画布暴露的命令式 API 与老画布同名（7 个）', () => {
  const oldExpose = exposeNames(OLD_CODE);
  const newExpose = exposeNames(NEW_CODE);
  assert.equal(oldExpose.length, 7, `老画布暴露方法数量变了：${oldExpose.join(', ')}`);
  assert.deepEqual(newExpose, oldExpose);
  // 切换器也要原样转出去（页面用的是 canvasRef.value?.fitView() 这类调用）
  const switchExpose = exposeNames(SWITCH_CODE);
  assert.deepEqual(switchExpose, oldExpose);
});

// ==================== ④ 视觉 / 几何约束 ====================

test('⑦ 节点卡片规格：11px 圆角 / 3.5px 色条 / 30×30 图标底块 两边都在', () => {
  for (const [label, src] of [
    ['老画布', OLD_CODE],
    ['新画布', NEW_CODE],
  ] as const) {
    assert.match(src, /border-radius: 11px/, `${label} 的卡片圆角不是 11px`);
    assert.match(src, /width: 3\.5px/, `${label} 的色条宽度不是 3.5px`);
    assert.match(src, /width: 30px;[\s\S]{0,120}?height: 30px/, `${label} 的图标底块不是 30×30`);
  }
});

test('⑧ Handle 方向：target=Left + source=Right（不给 Handle 会兜底成 bottom→top）', () => {
  assert.match(NEW_CODE, /type="target"[^>]*:position="Position\.Left"/, '新画布缺 target / Left 句柄');
  assert.match(NEW_CODE, /type="source"[^>]*:position="Position\.Right"/, '新画布缺 source / Right 句柄');
});

test('⑨ 连线几何仍复用 graph.ts / rounded-path.ts（不许另起一套坐标）', () => {
  assert.match(NEW_CODE, /import \{[^}]*edgePath[^}]*\} from '@\/views\/business\/flow\/graph'/, '新画布没复用 graph.ts 的 edgePath');
  assert.match(NEW_CODE, /roundedEdgePath\(edgePath\(/, '新画布没走「正交折线 → 圆角折线」这条链路');
});

test('⑪ 切换器与老画布的 props 默认值一致（boolean 缺省会被 Vue 转成 false，必须显式对齐）', () => {
  /** 取 `withDefaults(...)` 里的默认值对象（`name: value,`，跳过 `name?: type;` 声明行） */
  function defaultsOf(src: string): Record<string, string> {
    const start = src.indexOf('withDefaults(');
    assert.ok(start >= 0, 'withDefaults 没找到');
    const open = src.indexOf('{', src.indexOf('}>()', start));
    assert.ok(open > 0, 'withDefaults 的默认值对象没找到');
    let depth = 0;
    let end = -1;
    for (let i = open; i < src.length; i += 1) {
      if (src[i] === '{') depth += 1;
      else if (src[i] === '}') {
        depth -= 1;
        if (depth === 0) {
          end = i;
          break;
        }
      }
    }
    assert.ok(end > open, 'withDefaults 的默认值对象没闭合');
    const out: Record<string, string> = {};
    for (const match of src.slice(open, end).matchAll(/^\s*([A-Za-z_$][\w$]*):\s*([^,\n]+),/gm)) {
      out[match[1]] = match[2].trim();
    }
    return out;
  }
  const oldDefaults = defaultsOf(OLD_CODE);
  const switchDefaults = defaultsOf(SWITCH_CODE);
  assert.ok(Object.keys(oldDefaults).length >= 10, `老画布默认值解析异常：${JSON.stringify(oldDefaults)}`);
  for (const [name, value] of Object.entries(oldDefaults)) {
    assert.ok(name in switchDefaults, `切换器缺默认值 ${name}`);
    assert.equal(switchDefaults[name], value, `切换器默认值不一致：${name} ${switchDefaults[name]} ≠ ${value}`);
  }
});

test('⑫ 老画布一行没改：仍是被切换器导入的回退实现', () => {
  assert.match(SWITCH_CODE, /import NativeCanvas from '@\/views\/business\/flow\/components\/FlowCanvas\.vue'/, '切换器没把老画布作为回退');
  assert.match(SWITCH_CODE, /defineAsyncComponent\(\(\) => import\('@\/views\/business\/flow\/components\/FlowCanvasVueFlow\.vue'\)\)/, '新画布没做异步加载（会进首屏 chunk）');
});
