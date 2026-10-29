<template>
  <div class="ai-user-form">
    <!-- 状态徽标：只在编辑态显示（新建时 profile 还不存在） -->
    <div v-if="showStatus" class="ai-user-form__status">
      <t-space size="small" align="center" break-line>
        <t-tag :theme="statusTheme" variant="light">
          <template #icon><t-icon :name="statusIcon" /></template>
          {{ statusText }}
        </t-tag>
        <span class="ai-user-form__profile">profile：{{ profile || '—' }}</span>
        <t-button size="small" variant="text" :loading="statusLoading" @click="emit('refresh-status')">
          重新探活
        </t-button>
      </t-space>
      <div v-if="statusDetail" class="ai-user-form__detail">{{ statusDetail }}</div>
    </div>

    <t-form-item label="职责" name="agent_description">
      <t-input
        :value="description"
        :maxlength="255"
        placeholder="一句话说明这个 AI 用户负责什么（会写入 hermes profile describe）"
        @change="onDescription"
      />
    </t-form-item>

    <t-form-item v-if="showUsername" label="用户名" name="username">
      <div class="ai-user-form__username">
        <t-input
          :value="username"
          placeholder="小写字母开头，只含小写字母/数字/连字符，2–31 位"
          :status="usernameStatus"
          @change="onUsername"
        />
        <div class="ai-user-form__username-hint">
          该用户名同时是 Hermes profile 名（<code>^[a-z][a-z0-9-]{1,30}$</code>），
          <strong>创建后不可修改</strong>。
        </div>
      </div>
    </t-form-item>

    <t-form-item label="模型" name="model">
      <t-input
        :value="model"
        placeholder="可选，如 deepseek-v4.1-flash（留空则用 profile 自身配置）"
        @change="onModel"
      />
    </t-form-item>

    <t-form-item label="SOUL.md" name="soul">
      <SoulEditor
        :model-value="soul"
        :soul-path="soulPath"
        @update:model-value="emit('update:soul', $event)"
      />
    </t-form-item>
  </div>
</template>

<script setup lang="ts">
/**
 * AI 用户表单（新建 / 编辑共用）：职责 + 模型 + SOUL.md 编辑器 + profile 状态徽标。
 *
 * 与通用用户表单的分工：
 * - 显示名 / 部门 / 状态 → 通用用户表单（`sys_user` 的业务字段）；
 * - 职责 / SOUL.md / 模型 → 本表单（`sys_ai_agent` + Hermes profile）。
 *
 * 之所以不做「一个表单管两套」，是因为后端把两类字段拆成了两套接口
 * （`/system/user` 与 `/system/user/ai`），前端混在一起会让「保存到底调了哪个接口」
 * 变得不可预测 —— 而 AI 用户的保存还带磁盘副作用（写 SOUL.md）。
 */
import { computed } from 'vue';
import SoulEditor from './SoulEditor.vue';

const props = withDefaults(
  defineProps<{
    /** 表单模式 */
    mode: 'create' | 'edit';
    /** 是否由本组件渲染用户名输入（外层表单已有时置 false，避免两个输入框） */
    showUsername?: boolean;
    /** 是否显示状态徽标（新建时 profile 尚不存在，置 false） */
    showStatus?: boolean;
    /** 用户名（= profile 名，仅新建可填） */
    username?: string;
    /** 职责 */
    description?: string;
    /** 模型名（映射为 model_override.model） */
    model?: string;
    /** SOUL.md 全文 */
    soul?: string;
    /** SOUL.md 服务器绝对路径 */
    soulPath?: string;
    /** profile 名（编辑态展示） */
    profile?: string;
    /** 是否健康（目录 + SOUL.md + CLI 都在） */
    healthy?: boolean;
    /** profile 目录是否存在 */
    profileExists?: boolean;
    /** gateway 状态 */
    gateway?: string;
    /** 探活补充说明 */
    statusDetail?: string | null;
    /** 探活中 */
    statusLoading?: boolean;
  }>(),
  {
    username: '',
    description: '',
    model: '',
    soul: '',
    soulPath: '',
    profile: '',
    healthy: false,
    profileExists: false,
    gateway: 'unknown',
    statusDetail: null,
    statusLoading: false,
    showUsername: true,
    showStatus: true,
  },
);

const emit = defineEmits<{
  (e: 'update:username', value: string): void;
  (e: 'update:description', value: string): void;
  (e: 'update:model', value: string): void;
  (e: 'update:soul', value: string): void;
  (e: 'refresh-status'): void;
}>();

/**
 * t-input 的 change 载荷是 `InputValue`（string | number），这里统一转字符串再抛给父组件。
 *
 * 之所以包一层而不是在模板里直接 `emit('update:xxx', $event)`：
 * `$event` 的联合类型与 emit 的 `string` 形参不匹配（vue-tsc 会报 TS2769），
 * 且「数字输入框会给出 number」这件事必须在这里一次性收口，别让父组件猜。
 */
function onUsername(value: string | number): void {
  emit('update:username', String(value));
}

function onDescription(value: string | number): void {
  emit('update:description', String(value));
}

function onModel(value: string | number): void {
  emit('update:model', String(value));
}

/** 用户名即时校验（与后端正则逐字一致，减少「提交才报错」） */
const usernameStatus = computed<'success' | 'error' | 'default'>(() => {
  if (!props.username) {
    return 'default';
  }
  return /^[a-z][a-z0-9-]{1,30}$/.test(props.username) ? 'success' : 'error';
});

/**
 * 状态徽标文案（卡面要求三种：已创建 / 目录丢失 / 在线）。
 *
 * 优先级：目录丢失 > 在线 > 已创建（目录在但 gateway 未知）。
 */
const statusText = computed(() => {
  if (!props.profileExists) {
    return '目录丢失（profile 未创建或被外部删除）';
  }
  if (props.gateway === 'online') {
    return '在线';
  }
  if (props.gateway === 'offline') {
    return '已创建（gateway 未运行）';
  }
  return props.healthy ? '已创建' : '已创建（SOUL.md 缺失）';
});

const statusTheme = computed(() => {
  if (!props.profileExists) {
    return 'danger';
  }
  return props.gateway === 'online' ? 'success' : 'warning';
});

const statusIcon = computed(() => {
  if (!props.profileExists) {
    return 'error-circle';
  }
  return props.gateway === 'online' ? 'check-circle' : 'time';
});
</script>

<style scoped>
.ai-user-form {
  display: flex;
  flex-direction: column;
  gap: 4px;
  width: 100%;
}

.ai-user-form__status {
  margin-bottom: 8px;
  padding: 6px 10px;
  border-radius: 4px;
  background: var(--td-bg-color-secondarycontainer, #f3f3f3);
}

.ai-user-form__profile {
  font-size: 12px;
  color: var(--td-text-color-secondary);
  font-family: var(--td-font-family-mono, Menlo, Consolas, monospace);
}

.ai-user-form__detail {
  margin-top: 4px;
  font-size: 12px;
  color: var(--td-text-color-secondary);
  word-break: break-all;
}

.ai-user-form__username {
  display: flex;
  flex-direction: column;
  gap: 4px;
  width: 100%;
}

.ai-user-form__username-hint {
  font-size: 12px;
  color: var(--td-text-color-secondary);
  line-height: 1.5;
}

.ai-user-form__username-hint code {
  font-family: var(--td-font-family-mono, Menlo, Consolas, monospace);
  background: rgba(0, 0, 0, 0.04);
  padding: 0 4px;
  border-radius: 2px;
}
</style>
