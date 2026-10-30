<template>
  <!-- 单条件比较：field + cmp + value -->
  <div v-if="node.op === 'cmp'" class="ctn ct-num" :class="{ 'ctn--error': error !== undefined }">
    <div class="ctn__row">
      <span class="ctn__dash" aria-hidden="true">├</span>
      <t-select
        class="ctn__field"
        :value="node.field || undefined"
        :options="fieldOptions()"
        placeholder="字段：选固定字段，或直接输入 form.xxx"
        filterable
        creatable
        :disabled="readonly"
        @change="onFieldChange"
      />
      <t-select
        class="ctn__cmp"
        :value="node.cmp"
        :options="COMPARE_OPS"
        :disabled="readonly"
        @change="onCmpChange"
      />
      <t-input
        v-if="!isUnaryOp(node.cmp)"
        class="ctn__value"
        :value="valueText"
        :placeholder="valuePlaceholder"
        :disabled="readonly"
        @change="onValueChange"
        @blur="onValueBlur"
      />
      <span v-else class="ctn__unary">无需右值</span>
      <t-button
        v-if="!isRoot && !readonly"
        class="ctn__del"
        theme="default"
        variant="text"
        title="删除这个条件"
        @click="emit('remove', path)"
      >
        ×
      </t-button>
    </div>
    <p v-if="error !== undefined" class="ctn__error">{{ error }}</p>
  </div>

  <!-- 逻辑分组：and / or / not -->
  <div
    v-else
    class="ctn ct-group"
    :class="{ 'ctn--error': error !== undefined, 'ctn--root': isRoot }"
  >
    <div class="ctn__head">
      <span v-if="!isRoot" class="ctn__dash" aria-hidden="true">├</span>
      <t-select
        class="ctn__op"
        :value="node.op"
        :options="GROUP_OPS"
        :disabled="readonly"
        @change="onOpChange"
      />
      <span class="ctn__count">{{ node.args.length }} 项</span>
      <span class="ctn__depth" :class="{ 'ctn__depth--limit': depth >= MAX_DEPTH }">
        第 {{ depth }}/{{ MAX_DEPTH }} 层
      </span>
      <t-button
        v-if="!isRoot && !readonly"
        class="ctn__del"
        theme="default"
        variant="text"
        title="删除这个分组"
        @click="emit('remove', path)"
      >
        ×
      </t-button>
    </div>

    <div class="ctn__body">
      <ConditionNodeRow
        v-for="(child, index) in node.args"
        :key="index"
        :node="child"
        :path="[...path, index]"
        :depth="depth + 1"
        :readonly="readonly"
        :errors="errors"
        :blocks="blocks"
        @change="emit('change')"
        @remove="(target: number[]) => emit('remove', target)"
        @add="(groupId: number[], kind: 'cmp' | 'group') => emit('add', groupId, kind)"
      />

      <p v-if="node.args.length === 0" class="ctn__empty">
        {{ node.op === 'not' ? '「非」下需要一个条件' : '这一组还没有条件，点下面的按钮添加' }}
      </p>

      <div v-if="!readonly && node.op !== 'not'" class="ctn__actions">
        <t-button
          size="small"
          variant="outline"
          :disabled="block.cmp !== null"
          :title="block.cmp ?? ''"
          @click="emit('add', path, 'cmp')"
        >
          + 添加条件
        </t-button>
        <t-button
          size="small"
          variant="outline"
          :disabled="block.group !== null"
          :title="block.group ?? ''"
          @click="emit('add', path, 'group')"
        >
          + 添加分组
        </t-button>
        <span v-if="block.cmp !== null || block.group !== null" class="ctn__block">
          {{ block.cmp ?? block.group }}
        </span>
      </div>
    </div>

    <p v-if="error !== undefined" class="ctn__error">{{ error }}</p>
  </div>
</template>

<script setup lang="ts">
/**
 * 条件树的一行（递归组件）。
 *
 * # 为什么直接改 node
 *
 * `node` 是父级（`ConditionTree.vue`）里 `ref` 包裹的响应式树的子对象 —— 改它的字段就是改同一份状态，
 * 不需要 `v-model` 逐层透传；改完抛一次 `change`，由根组件统一 `toJson()` 后交给 `patch-edge`。
 * 这样「撤销栈」里只压一条边缘命令，不会被中间层拆成多次。
 *
 * # 上限只读不写
 *
 * 「还能不能加」由根组件算好塞在 `blocks` 里（`condition-tree.ts::addBlock`），
 * 这里只负责把按钮 disabled 掉并把原因挂在 `title` 上 —— 单一判定来源，避免两处各算一遍。
 */
import {
  COMPARE_OPS,
  GROUP_OPS,
  MAX_DEPTH,
  fieldOptions,
  isArrayOp,
  isUnaryOp,
  literalFromInput,
  literalToInput,
  pathKey,
  type AddBlock,
  type CmpNode,
  type CompareOp,
  type ConditionLiteral,
  type ConditionNode,
  type GroupNode,
  type GroupOp,
} from './condition-tree';

/** 组件入参 */
const props = withDefaults(
  defineProps<{
    /** 本行节点 */
    node: ConditionNode;
    /** 从根到本行的下标路径 */
    path: number[];
    /** 层数（根 = 1） */
    depth: number;
    /** 只读（已发布版本） */
    readonly?: boolean;
    /** `path.join('.') → 第一条错误原因` */
    errors?: Record<string, string>;
    /** `path.join('.') → 该分组还能不能加子节点` */
    blocks?: Record<string, AddBlock>;
  }>(),
  { readonly: false, errors: () => ({}), blocks: () => ({}) },
);

/** 组件事件：改内容 / 删节点 / 加子节点（统一冒泡到根组件） */
const emit = defineEmits<{
  (event: 'change'): void;
  (event: 'remove', path: number[]): void;
  (event: 'add', groupPath: number[], kind: 'cmp' | 'group'): void;
}>();

/** 是否根节点（根不显示删除按钮，改由头部「清空」处理） */
const isRoot = computed<boolean>(() => props.path.length === 0);

/** 本行的第一条错误 */
const error = computed<string | undefined>(() => props.errors[pathKey(props.path)]);

/** 本分组的「还能不能加」（比较节点用不到，给个全挡住的默认值） */
const block = computed<AddBlock>(
  () => props.blocks[pathKey(props.path)] ?? { cmp: '这里不能加子条件', group: '这里不能加子条件' },
);

/** 右值输入框的本地草稿 —— 只在用户敲字期间存在 */
const valueDraft = ref<string | null>(null);

/**
 * 右值输入框显示什么。
 *
 * 为什么要有草稿：如果把「文本 → 字面量 → 规范化文本」回写到 `:value`，每敲一个字都会重排文本，
 * 打 `100,` 时逗号会被 `literalToInput` 吃掉（变成 `100`），接着打 `200` 就成了 `100200` —— 实测踩过。
 * 所以编辑期显示用户原样输入，失焦后再换成规范化文本（`100,200` → `100, 200`）。
 */
const valueText = computed<string>(() =>
  valueDraft.value ?? (props.node.op === 'cmp' ? literalToInput(props.node.value) : ''),
);

/** 右值输入框占位文案（`in` 提示逗号分隔） */
const valuePlaceholder = computed<string>(() => {
  if (props.node.op !== 'cmp') return '';
  if (isArrayOp(props.node.cmp)) return '多个值用逗号分隔，如 100, 200';
  if (props.node.cmp === 'contains') return '子串，如 报销';
  return '右值：数字 / 文本 / true / false';
});

/** 改字段（`creatable` 下用户也可以直接敲 `form.amount`） */
function onFieldChange(value: unknown): void {
  if (props.node.op !== 'cmp') return;
  props.node.field = String(value ?? '').trim();
  emit('change');
}

/** 改比较符：顺手把右值转成新算子要的形状，别留一个 `[1,2]` 给 `=` */
function onCmpChange(value: unknown): void {
  if (props.node.op !== 'cmp') return;
  const node: CmpNode = props.node;
  const next = String(value ?? 'eq') as CompareOp;
  node.cmp = next;
  valueDraft.value = null; // 算子变了 → 丢掉草稿，显示转换后的规范文本
  if (isUnaryOp(next)) {
    node.value = null;
  } else if (isArrayOp(next)) {
    if (!Array.isArray(node.value)) {
      const single = literalToInput(node.value);
      node.value = literalFromInput(single, next).value;
    }
  } else if (Array.isArray(node.value)) {
    const first = node.value[0];
    node.value = (first ?? '') as ConditionLiteral;
  }
  emit('change');
}

/** 改右值：文本 → JSON 字面量（非法就让体检标红，不阻断输入） */
function onValueChange(value: unknown): void {
  if (props.node.op !== 'cmp') return;
  const text = String(value ?? '');
  valueDraft.value = text;
  const parsed = literalFromInput(text, props.node.cmp);
  props.node.value = parsed.value;
  emit('change');
}

/** 右值失焦：丢掉草稿，显示规范化后的文本 */
function onValueBlur(): void {
  valueDraft.value = null;
}

/** 改分组算子：`not` 只留第一个子条件 */
function onOpChange(value: unknown): void {
  if (props.node.op === 'cmp') return;
  const node: GroupNode = props.node;
  const next = String(value ?? 'and') as GroupOp;
  node.op = next;
  if (next === 'not' && node.args.length > 1) node.args = node.args.slice(0, 1);
  emit('change');
}
</script>

<style scoped lang="less">
.ctn {
  &__row,
  &__head {
    display: flex;
    gap: 6px;
    align-items: center;
    padding: 4px 0;
  }

  &__head {
    padding-bottom: 2px;
  }

  &__dash {
    flex: none;
    width: 10px;
    font-size: 12px;
    line-height: 1;
    color: var(--td-text-color-placeholder);
  }

  &__field {
    flex: 1 1 46%;
    min-width: 0;
  }

  &__cmp {
    flex: none;
    width: 118px;
  }

  &__value {
    flex: 1 1 34%;
    min-width: 0;
  }

  &__unary {
    flex: 1 1 34%;
    display: flex;
    align-items: center;
    height: 32px;
    padding: 0 11px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
    border: 1px dashed var(--td-component-stroke);
    border-radius: var(--td-radius-default);
  }

  &__op {
    flex: none;
    width: 108px;
  }

  &__count,
  &__depth {
    flex: none;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__depth--limit {
    color: var(--td-error-color);
    font-weight: 600;
  }

  &__del {
    flex: none;
    min-width: 24px;
    padding: 0 4px;
    font-size: 15px;
    line-height: 1;
    color: var(--td-text-color-placeholder);

    &:hover {
      color: var(--td-error-color);
    }
  }

  &__body {
    padding-left: 10px;
    margin-left: 4px;
    border-left: 1px solid var(--td-component-stroke);
  }

  &__actions {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: center;
    padding: 6px 0 2px;
  }

  &__block {
    font-size: 12px;
    color: var(--td-error-color);
  }

  &__empty {
    padding: 2px 0 4px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__error {
    padding: 2px 0 4px 16px;
    font-size: 12px;
    line-height: 1.5;
    color: var(--td-error-color);
  }

  /* 超限/非法：整行红框（只用主题 token，不写裸 hex —— 22 套配色的门禁要求） */
  &--error {
    .ctn__body {
      border-left-color: var(--td-error-color);
    }
  }

  &.ctn--error > .ctn__row,
  &.ctn--error > .ctn__head {
    padding: 4px 6px;
    margin-left: -6px;
    border: 1px solid var(--td-error-color);
    border-radius: var(--td-radius-default);
    background: var(--td-error-color-1);
  }
}
</style>
