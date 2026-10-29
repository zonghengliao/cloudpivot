<template>
  <div class="soul-editor">
    <div class="soul-editor__bar">
      <t-space size="small" align="center">
        <span class="soul-editor__title">SOUL.md</span>
        <t-tag size="small" variant="light" theme="default">{{ charCount }} 字符 / {{ byteCount }} 字节</t-tag>
      </t-space>
      <t-space size="small">
        <t-radio-group v-model="mode" variant="default-filled" size="small">
          <t-radio-button value="edit">编辑</t-radio-button>
          <t-radio-button value="split">分栏</t-radio-button>
          <t-radio-button value="preview">预览</t-radio-button>
        </t-radio-group>
        <t-button size="small" variant="outline" @click="fillTemplate">用模板填充</t-button>
      </t-space>
    </div>

    <div class="soul-editor__body" :class="`soul-editor__body--${mode}`">
      <div v-show="mode !== 'preview'" class="soul-editor__pane">
        <t-textarea
          :value="modelValue"
          class="soul-editor__textarea"
          :autosize="{ minRows: 14, maxRows: 26 }"
          placeholder="# 身份 / 核心职责 / 所属团队 / 工作原则 / 个性特点……"
          @change="onInput"
        />
      </div>
      <div v-show="mode !== 'edit'" class="soul-editor__pane soul-editor__preview">
        <div v-if="lines.length === 0" class="soul-editor__empty">（还没有内容）</div>
        <template v-else>
          <component
            :is="line.kind === 'h1' ? 'h3' : line.kind === 'h2' ? 'h4' : line.kind === 'h3' ? 'h5' : 'p'"
            v-for="(line, index) in lines"
            :key="index"
            :class="['soul-line', `soul-line--${line.kind}`]"
          >
            <span v-if="line.kind === 'li'" class="soul-line__dot">•</span>{{ line.text }}
          </component>
        </template>
      </div>
    </div>

    <!-- 保存前提示：把「会写到哪台机器的哪个文件」说清楚（卡面硬要求） -->
    <div class="soul-editor__hint">
      <t-icon name="info-circle" />
      <span>
        保存后将<strong>直接写入</strong>
        <code>{{ soulPath || '服务器上的 /root/.hermes/profiles/&lt;name&gt;/SOUL.md' }}</code>
        （经 Hermes 官方目录，非数据库副本）。此内容会进入 AI 的每次会话上下文，请谨慎编辑。
      </span>
    </div>
  </div>
</template>

<script setup lang="ts">
/**
 * SOUL.md 编辑器：等宽编辑 + Markdown 结构预览 + 字数统计 + 模板填充。
 *
 * 设计取舍（安全）：
 * - 预览**不用 `v-html`**：SOUL.md 是管理员手写的内容，但一旦用 HTML 渲染就必须依赖
 *   净化器；这里按行解析成标题/列表/段落并用 Vue 插值渲染（自动转义），
 *   因此 `<script>` 之类的输入只会以文本出现，不存在 XSS 面。
 * - 不支持行内 Markdown（粗体/链接）：预览的目的是「看出结构对不对」，
 *   不是所见即所得编辑器；过度实现反而会让人误以为保存的就是渲染结果。
 */
import { computed, ref } from 'vue';

const props = withDefaults(
  defineProps<{
    /** SOUL.md 全文 */
    modelValue: string;
    /** SOUL.md 在服务器上的绝对路径（保存前提示用，编辑态由后端下发） */
    soulPath?: string;
    /** 默认视图 */
    defaultMode?: 'edit' | 'split' | 'preview';
  }>(),
  { soulPath: '', defaultMode: 'split' },
);

const emit = defineEmits<{
  'update:modelValue': [value: string];
}>();

/** 视图模式（纯本地状态，不对外暴露） */
const mode = ref<'edit' | 'split' | 'preview'>(props.defaultMode);

/** 字符数（按 Unicode 码点，中文一个字算一个） */
const charCount = computed(() => Array.from(props.modelValue || '').length);
/** 字节数（UTF-8，后端上限 256 KiB 的口径） */
const byteCount = computed(() => new TextEncoder().encode(props.modelValue || '').length);

/** 预览用行模型 */
interface PreviewLine {
  /** 行类型：h1/h2/h3 标题、li 列表项、p 段落 */
  kind: 'h1' | 'h2' | 'h3' | 'li' | 'p';
  /** 行文本（已去掉 Markdown 标记） */
  text: string;
}

const lines = computed<PreviewLine[]>(() => {
  const raw = props.modelValue || '';
  const out: PreviewLine[] = [];
  for (const line of raw.split('\n')) {
    const trimmed = line.trim();
    if (!trimmed) {
      continue;
    }
    if (trimmed.startsWith('### ')) {
      out.push({ kind: 'h3', text: trimmed.slice(4) });
    } else if (trimmed.startsWith('## ')) {
      out.push({ kind: 'h2', text: trimmed.slice(3) });
    } else if (trimmed.startsWith('# ')) {
      out.push({ kind: 'h1', text: trimmed.slice(2) });
    } else if (trimmed.startsWith('- ') || trimmed.startsWith('* ')) {
      out.push({ kind: 'li', text: trimmed.slice(2) });
    } else {
      out.push({ kind: 'p', text: trimmed });
    }
  }
  return out;
});

/** t-textarea 的 change 载荷是 `TextareaValue`（string | number），统一转成字符串 */
function onInput(value: string | number): void {
  emit('update:modelValue', String(value));
}

/**
 * 「用模板填充」：生成与后端 `render_default_soul` 同构的五段结构。
 *
 * 后端在 SOUL.md 留空时也会自动生成同结构内容，这里提供按钮只是让操作者
 * 「看得见将要生成什么、并且能先改」。
 */
function fillTemplate(): void {
  const template = [
    '你是<角色名>，<一句话说明这个 AI 用户负责什么>。',
    '你运行在 Hermes Agent（由 Nous Research 打造）之上。',
    '# 核心职责',
    '- <职责 1>',
    '- <职责 2>',
    '# 所属团队',
    '由云枢创建并托管的 AI 用户。职责与人格在云枢「用户管理」中维护。',
    '# 工作原则',
    '- 中文为主要交流语言',
    '- 交付物必须是基于真实工具输出的可工作成果，而不是描述',
    '- 数据、结论要给出依据和来源，不确定时坦诚说明',
    '# 个性特点',
    '- <说话风格 / 协作偏好>',
    '',
  ].join('\n');
  emit('update:modelValue', template);
}
</script>

<style scoped>
.soul-editor {
  display: flex;
  flex-direction: column;
  gap: 8px;
  width: 100%;
}

.soul-editor__bar {
  display: flex;
  align-items: center;
  justify-content: space-between;
  flex-wrap: wrap;
  gap: 8px;
}

.soul-editor__title {
  font-weight: 600;
  font-size: 13px;
  color: var(--td-text-color-primary);
}

.soul-editor__body {
  display: flex;
  gap: 8px;
  align-items: stretch;
}

.soul-editor__body--edit .soul-editor__pane,
.soul-editor__body--preview .soul-editor__pane {
  flex: 1 1 100%;
}

.soul-editor__body--split .soul-editor__pane {
  flex: 1 1 50%;
  min-width: 0;
}

.soul-editor__pane {
  display: flex;
  flex-direction: column;
}

.soul-editor__textarea :deep(textarea) {
  font-family: var(--td-font-family-mono, 'JetBrains Mono', Menlo, Consolas, monospace);
  font-size: 12px;
  line-height: 1.6;
}

.soul-editor__preview {
  border: 1px solid var(--td-component-stroke);
  border-radius: 4px;
  padding: 8px 12px;
  min-height: 200px;
  max-height: 480px;
  overflow: auto;
  background: var(--td-bg-color-container);
}

.soul-editor__empty {
  color: var(--td-text-color-placeholder);
  font-size: 12px;
}

.soul-line {
  margin: 0 0 4px;
  font-size: 13px;
  line-height: 1.6;
  color: var(--td-text-color-primary);
  word-break: break-word;
}

.soul-line--h1 {
  font-size: 15px;
  font-weight: 600;
  margin-top: 8px;
}

.soul-line--h2 {
  font-size: 14px;
  font-weight: 600;
  margin-top: 6px;
}

.soul-line--h3 {
  font-size: 13px;
  font-weight: 600;
}

.soul-line--li {
  padding-left: 14px;
  position: relative;
}

.soul-line__dot {
  position: absolute;
  left: 2px;
  color: var(--td-brand-color);
}

.soul-editor__hint {
  display: flex;
  gap: 6px;
  align-items: flex-start;
  font-size: 12px;
  line-height: 1.5;
  color: var(--td-warning-color, #d58400);
  background: var(--td-warning-color-light, #fff1e9);
  border-radius: 4px;
  padding: 6px 10px;
}

.soul-editor__hint code {
  font-family: var(--td-font-family-mono, Menlo, Consolas, monospace);
  background: rgba(0, 0, 0, 0.04);
  padding: 0 4px;
  border-radius: 2px;
  word-break: break-all;
}
</style>
