<template>
  <!--
    模板变量选择器（设计 §4.2）。

    # 两种模式（**按模式切换，不许混** —— 决策 3，红线）

    | `mode` | 显示什么 | 插入什么 |
    |---|---|---|
    | `inline`（默认） | 分组树（业务单据 / 发起人 / 本节点 / 上一审批人 / 表单字段 / 任务字段） | 点号路径 `{{instance.biz_no}}`，走后端 `render_paths` |
    | `template-ref` | **不渲染选择器**，只一行只读提示「本模板可用变量：biz_no、node_name…」 | 什么都不插（那些是无点号名，走旧 `render`，插点号会 422） |

    第二种模式存在的理由：`sys_notice_template.variables` 的变量名文法被 `render()` 锁死在
    「无点号」。在「引用 template_code」的地方给点号选择器，用户点一下就把模板弄成 422。
    正确做法是**根本不给**选择器，并告诉他模板管理页里已经声明了哪些变量。
  -->
  <span v-if="mode === 'template-ref'" class="var-picker__ref-hint">
    本模板可用变量：<code>{{ refVarsText }}</code>
    <span class="var-picker__ref-tip">（无点号名，由通知模板管理页维护，本处不可插入）</span>
  </span>

  <t-popup
    v-else
    v-model:visible="open"
    trigger="click"
    placement="bottom-left"
    :disabled="readonly"
    :overlay-inner-class="'var-picker__panel-wrap'"
    @visible-change="onVisibleChange"
  >
    <t-button size="small" variant="outline" :disabled="readonly" class="var-picker__trigger">
      变量
      <template #suffix><t-icon name="chevron-down" /></template>
    </t-button>

    <template #content>
      <div class="var-picker" data-testid="template-var-picker">
        <div v-for="group in groups" :key="group.key" class="var-picker__group">
          <div class="var-picker__group-title">{{ group.label }}</div>
          <div v-if="group.options.length === 0" class="var-picker__empty">
            {{ group.key === 'form' ? formEmptyText : '本组无可用变量' }}
          </div>
          <button
            v-for="option in group.options"
            :key="option.path"
            type="button"
            class="var-picker__item"
            :data-var-path="option.path"
            @click="pick(option.path)"
          >
            <span class="var-picker__label">{{ option.label }}</span>
            <code class="var-picker__path">{{ option.path }}</code>
          </button>
        </div>

        <!-- 表单字段：没有 form-fields 接口时**只允许手输**，绝不伪造候选 -->
        <div class="var-picker__manual">
          <t-input
            v-model="manualField"
            size="small"
            placeholder="手输表单字段名，如 supplier_name"
            :maxlength="64"
            @enter="pickManual"
          />
          <t-button size="small" theme="primary" variant="outline" :disabled="!manualField.trim()" @click="pickManual">
            插入 form.{{ manualField.trim() || '字段名' }}
          </t-button>
          <p class="var-picker__manual-tip">
            表单字段候选来自历史实例的 form_data（本版本没有 form-fields 接口）；
            没列出来的字段请直接手输，插入后得到 &#123;&#123;form.字段名&#125;&#125; 形式的占位符。
          </p>
        </div>
      </div>
    </template>
  </t-popup>
</template>

<script setup lang="ts">
/**
 * 变量选择器（设计 §4.2 / §2.5.1）。
 *
 * # 职责边界
 *
 * 只做两件事：**列出可插的路径**（{@link varGroups} 给的，判据在 `template-vars.ts`）、
 * **把选中的路径抛出去**（`insert` 事件）。插入位置与光标由父组件负责 ——
 * 只有它拿得到那个 `<textarea>` 的 `selectionStart`。
 *
 * # 为什么 `form.*` 允许手输
 *
 * 设计 §4.2 第 2 条：候选来自流程表单定义，但 `biz_flow_def` 上没存表单 schema，
 * 也没有 `form-fields` 接口。**宁可少列，不可列错**（伪造候选最坏的结果是
 * 用户照着点、发布检查报 65037，或者运行期渲染成「缺变量」）。
 * 所以这里只列**真实见过的键** + 手输入口。
 */
import { computed, ref } from 'vue';
import { varGroups } from '@/views/business/flow/components/template-vars';

/** 组件入参 */
const props = withDefaults(
  defineProps<{
    /** 本流程业务类型（决定 `task.*` 这组显不显示） */
    bizType?: string | null;
    /** `form.*` 的候选字段名（**只放真实见过的**；空数组 = 只给手输） */
    formFields?: string[];
    /** 模式：内联模板（插点号）/ 引用 template_code（只读提示） */
    mode?: 'inline' | 'template-ref';
    /** 只读（已发布版本） */
    readonly?: boolean;
    /** `template-ref` 模式下的只读变量名 */
    refVars?: string[];
  }>(),
  {
    bizType: null,
    formFields: () => [],
    mode: 'inline',
    readonly: false,
    refVars: () => ['biz_no', 'instance_title', 'initiator_name', 'node_name'],
  },
);

/** 组件事件 */
const emit = defineEmits<{
  /** 选中一个变量路径（父组件负责在光标处插 `{{path}}`） */
  (event: 'insert', path: string): void;
}>();

/** 弹层开关（受控，选完即关） */
const open = ref(false);

/** 手输的表单字段名 */
const manualField = ref('');

/** 只读提示里的变量名 */
const refVarsText = computed(() => props.refVars.join('、'));

/** 分组菜单（顺序 = 设计 §4.2 的树） */
const groups = computed(() => varGroups({ bizType: props.bizType, formFields: props.formFields }));

/** 表单字段组为空时的说明（区分「没历史实例」与「实例里没有 form_data」） */
const formEmptyText = computed(() =>
  props.formFields.length === 0
    ? '暂无候选（本流程还没有带表单数据的历史实例），请在下方手输字段名'
    : '',
);

/** 选中一个现成变量 */
function pick(path: string): void {
  emit('insert', path);
  open.value = false;
}

/** 插入手输的表单字段 */
function pickManual(): void {
  const field = manualField.value.trim();
  if (!field) {
    return;
  }
  emit('insert', `form.${field}`);
  manualField.value = '';
  open.value = false;
}

/** 打开时清掉上一轮的手输内容（避免误插上一次的字段） */
function onVisibleChange(visible: boolean): void {
  if (visible) {
    manualField.value = '';
  }
}
</script>

<style scoped lang="less">
.var-picker {
  display: flex;
  flex-direction: column;
  gap: 10px;
  width: 320px;
  max-height: 420px;
  padding: 10px 12px;
  overflow-y: auto;

  &__group {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  &__group-title {
    padding: 4px 0 2px;
    font-size: 11px;
    font-weight: 600;
    color: var(--td-text-color-placeholder);
    letter-spacing: 0.02em;
  }

  &__item {
    display: flex;
    gap: 8px;
    align-items: center;
    justify-content: space-between;
    padding: 5px 6px;
    font: inherit;
    text-align: left;
    cursor: pointer;
    background: none;
    border: none;
    border-radius: 4px;

    &:hover {
      background: var(--td-bg-color-container-hover);
    }
  }

  &__label {
    font-size: 13px;
    color: var(--td-text-color-primary);
  }

  &__path {
    font-size: 11px;
    color: var(--td-text-color-placeholder);
  }

  &__empty {
    padding: 4px 6px;
    font-size: 11px;
    line-height: 1.5;
    color: var(--td-warning-color);
  }

  &__manual {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding-top: 8px;
    border-top: 1px dashed var(--td-component-stroke);
  }

  &__manual-tip {
    margin: 0;
    font-size: 11px;
    line-height: 1.5;
    color: var(--td-text-color-placeholder);
  }

  &__ref-hint {
    display: inline-flex;
    flex-wrap: wrap;
    gap: 4px;
    align-items: baseline;
    font-size: 12px;
    color: var(--td-text-color-secondary);
  }

  &__ref-tip {
    color: var(--td-text-color-placeholder);
  }
}

.var-picker__trigger {
  flex: none;
}
</style>
