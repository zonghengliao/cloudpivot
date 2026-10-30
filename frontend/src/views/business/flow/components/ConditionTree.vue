<template>
  <div class="ct" :class="{ 'ct--error': !ok }">
    <!-- 头部：计数 + 上限提示 + 清空 -->
    <div class="ct__head">
      <span class="ct__title">条件规则</span>
      <span class="ct__meter" :class="{ 'ct__meter--limit': report.nodes > MAX_NODES }">
        节点 {{ report.nodes }}/{{ MAX_NODES }}
      </span>
      <span class="ct__meter" :class="{ 'ct__meter--limit': report.depth > MAX_DEPTH }">
        深度 {{ report.depth }}/{{ MAX_DEPTH }}
      </span>
      <span class="ct__spacer" />
      <t-button
        v-if="!readonly"
        size="small"
        variant="text"
        :disabled="tree === null"
        @click="onClear"
      >
        清空
      </t-button>
    </div>

    <!-- 空态：无条件 = 总是通过 -->
    <div v-if="tree === null" class="ct__empty">
      <p class="ct__empty-text">未设置条件 —— 这条连线无条件通过。</p>
      <div v-if="!readonly" class="ct__empty-actions">
        <t-button size="small" variant="outline" @click="onAddFirst('cmp')">
          + 添加条件
        </t-button>
        <t-button size="small" variant="outline" @click="onAddFirst('group')">
          + 添加分组（and / or）
        </t-button>
      </div>
    </div>

    <!-- 规则树 -->
    <template v-else>
      <ConditionNodeRow
        :node="tree"
        :path="[]"
        :depth="1"
        :readonly="readonly"
        :errors="errors"
        :blocks="blocks"
        @change="commit"
        @remove="onRemove"
        @add="onAdd"
      />
    </template>

    <!-- 错误汇总（超限 / 白名单 / 形状） -->
    <ul v-if="issues.length > 0" class="ct__issues">
      <li v-for="(issue, index) in issues.slice(0, 4)" :key="index" class="ct__issue">
        {{ issue.message }}
      </li>
      <li v-if="issues.length > 4" class="ct__issue">… 还有 {{ issues.length - 4 }} 条</li>
    </ul>

    <!-- JSON 预览：给后端 `parse_condition` 的原文，方便排障 -->
    <div class="ct__foot">
      <t-button size="small" variant="text" @click="showJson = !showJson">
        {{ showJson ? '收起 JSON' : '查看 JSON' }}
      </t-button>
      <span class="ct__foot-hint">提交时按 op / field / cmp / value / args 生成</span>
    </div>
    <pre v-if="showJson" class="ct__json">{{ jsonText }}</pre>
  </div>
</template>

<script setup lang="ts">
/**
 * when 条件树编辑器（批次 D · P1-B1）。
 *
 * # 双向绑定
 *
 * `modelValue` 就是后端 `edge.condition`（`unknown`，可能是 `null` / 老版本 JSON）。
 * 进来先过 `fromJson()` 反解成规则树，出去过 `toJson()` 生成**键名固定**的 JSON
 * （op / field / cmp / value / args），与 `graph.rs::parse_condition` 逐字兼容。
 *
 * # 为什么不是每个字段一次 emit
 *
 * 属性面板把每条命令压进撤销栈（见 `NodeProps.vue` 头注释）。所以这里所有改动都收敛成
 * **一次** `update:modelValue`，一次编辑 = 一条可撤销命令。
 *
 * # 上限
 *
 * 深度 ≤ 8 / 节点 ≤ 64 / 每组 args ≤ 16 全部来自 `condition-tree.ts`（对齐后端常量）；
 * 越界的按钮直接 disabled 并在下方红字说明原因，不让用户拼出一条后端必拒的表达式。
 */
import { ref, watch } from 'vue';
import ConditionNodeRow from './ConditionNodeRow.vue';
import {
  MAX_DEPTH,
  MAX_NODES,
  addBlock,
  addChild,
  createCmpNode,
  createGroupNode,
  fromJson,
  issueIndex,
  pathKey,
  removeAt,
  semanticKey,
  toJson,
  toJsonText,
  validateTree,
  type AddBlock,
  type ConditionJson,
  type ConditionNode,
  type TreeIssue,
} from './condition-tree';

/** 组件入参 */
const props = withDefaults(
  defineProps<{
    /** 后端 `condition` 原文（null = 无条件） */
    modelValue?: unknown;
    /** 只读（已发布版本） */
    readonly?: boolean;
  }>(),
  { readonly: false },
);

/** 组件事件：整棵树 → JSON */
const emit = defineEmits<{
  (event: 'update:modelValue', value: ConditionJson | null): void;
}>();

/** 本地规则树（编辑期唯一真源） */
const tree = ref<ConditionNode | null>(null);
/** 反解 JSON 时发现的问题（后端存进来的脏数据） */
const parseIssues = ref<TreeIssue[]>([]);
/** 是否展开 JSON 预览 */
const showJson = ref(false);

/** 反解：JSON → 树 */
function rebuild(raw: unknown): void {
  const parsed = fromJson(raw);
  tree.value = parsed.node;
  parseIssues.value = parsed.issues;
}

/**
 * 回灌判断：只有「外部给的值 ≠ 当前树生成的值」才重建，
 * 否则每次 emit 都会把用户正在编辑的输入框重置掉（光标跳走）。
 */
watch(
  () => props.modelValue,
  (value) => {
    if (semanticKey(value) === semanticKey(toJson(tree.value))) return;
    rebuild(value);
  },
  { immediate: true },
);

/** 全树体检（字段白名单 + 上限 + 形状） */
const report = computed(() => validateTree(tree.value));

/** 所有问题（反解问题在前） */
const issues = computed<TreeIssue[]>(() => [...parseIssues.value, ...report.value.issues]);

/** 体检通过 */
const ok = computed<boolean>(() => issues.value.length === 0);

/** `path → 第一条错误`，行内红框用 */
const errors = computed<Record<string, string>>(() => issueIndex(report.value));

/** `path → 该分组还能不能加子节点`，按钮 disabled 用 */
const blocks = computed<Record<string, AddBlock>>(() => {
  const map: Record<string, AddBlock> = {};
  const walk = (node: ConditionNode | null, path: number[]): void => {
    if (!node || node.op === 'cmp') return;
    map[pathKey(path)] = addBlock(tree.value, path);
    node.args.forEach((child, index) => walk(child, [...path, index]));
  };
  walk(tree.value, []);
  return map;
});

/** JSON 预览文本 */
const jsonText = computed<string>(() => toJsonText(tree.value));

/** 提交：整树 → JSON（空树 = null = 无条件） */
function commit(): void {
  emit('update:modelValue', toJson(tree.value));
}

/** 加子节点（越界时组件已 disable，这里再兜一层） */
function onAdd(groupPath: number[], kind: 'cmp' | 'group'): void {
  const added = addChild(tree.value, groupPath, kind);
  if (added === null) return;
  commit();
}

/** 删节点 */
function onRemove(path: number[]): void {
  tree.value = removeAt(tree.value, path);
  commit();
}

/** 清空 = 无条件 */
function onClear(): void {
  tree.value = null;
  parseIssues.value = [];
  commit();
}

/** 空态下造第一个节点 */
function onAddFirst(kind: 'cmp' | 'group'): void {
  tree.value = kind === 'cmp' ? createCmpNode() : createGroupNode('and');
  commit();
}
</script>

<style scoped lang="less">
.ct {
  padding: 8px 10px 6px;
  border: 1px solid var(--td-component-stroke);
  border-radius: var(--td-radius-default);
  background: var(--td-bg-color-container);

  &--error {
    border-color: var(--td-error-color);
  }

  &__head {
    display: flex;
    gap: 8px;
    align-items: center;
    padding-bottom: 6px;
    border-bottom: 1px dashed var(--td-component-stroke);
  }

  &__title {
    font-size: 13px;
    font-weight: 600;
    color: var(--td-text-color-primary);
  }

  &__meter {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__meter--limit {
    font-weight: 600;
    color: var(--td-error-color);
  }

  &__spacer {
    flex: 1;
  }

  &__empty {
    padding: 10px 0 4px;
  }

  &__empty-text {
    margin-bottom: 8px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__empty-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  &__issues {
    margin: 6px 0 0;
    padding: 6px 8px 6px 22px;
    list-style: disc;
    border-radius: var(--td-radius-default);
    background: var(--td-error-color-1);
  }

  &__issue {
    font-size: 12px;
    line-height: 1.6;
    color: var(--td-error-color);
  }

  &__foot {
    display: flex;
    gap: 8px;
    align-items: center;
    padding-top: 4px;
    margin-top: 4px;
    border-top: 1px dashed var(--td-component-stroke);
  }

  &__foot-hint {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__json {
    max-height: 220px;
    padding: 8px;
    margin: 4px 0 0;
    overflow: auto;
    font-family: var(--td-font-family-mono, monospace);
    font-size: 12px;
    line-height: 1.55;
    color: var(--td-text-color-secondary);
    background: var(--td-bg-color-secondarycontainer);
    border-radius: var(--td-radius-default);
    white-space: pre-wrap;
    word-break: break-all;
  }
}
</style>
