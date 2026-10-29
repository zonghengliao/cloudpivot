<template>
  <t-select
    :value="modelValue"
    :options="options"
    :placeholder="placeholder"
    :loading="loading"
    multiple
    clearable
    filterable
    :min-collapsed-num="2"
    class="mention-select"
    @update:value="onChange"
    @search="onSearch"
  />
</template>

<script setup lang="ts">
/**
 * 用户多选（评论 `@` 提及专用）。
 *
 * # 为什么不复用 `UserSelect.vue`
 *
 * 那个是**单选**（负责人 / 筛选条件），`modelValue` 是 `string | null`。
 * 给它加 `multiple` 会让绑定的类型变成联合类型，`v-model` 挂在
 * `string` 字段上的既有页面（列表筛选 / 任务表单）立刻过不了 `vue-tsc`。
 * 这里另起一个只做多选的小组件，交互面（已选多少、怎么搜）也完全不同。
 *
 * # 搜索口径与 `UserSelect` 一致
 *
 * `GET /api/system/user/page` 的 `username` 与 `real_name` 是 **AND** 关系，
 * 一个关键词没法同时命中两者 → 并发发两次再按 id 合并去重。
 *
 * # 被选中的人会收到什么
 *
 * 强制站内通知（修正 8：被 @ = 强制，不看收件人偏好开关）。
 * 通知扇出在后端 `business/task/collab.rs::notify_mentions`，前端只负责传 id。
 */
import { ref } from 'vue';
import { pageUsers, type UserRes } from '@/api/system/user-api';

/**
 * 声明 props 时**不接赋值**：本组件的 props 只出现在模板里（`modelValue` /
 * `placeholder`），`<script setup>` 里不需要 `props` 变量 —— 接了反而会被
 * `vue-tsc` 判成「声明未使用」（TS6133）。
 */
withDefaults(
  defineProps<{
    /** 已选用户 id 列表 */
    modelValue?: string[];
    /** 占位文案 */
    placeholder?: string;
  }>(),
  {
    modelValue: () => [],
    placeholder: '@ 提醒同事（可多选）',
  },
);

const emit = defineEmits<{
  /** 选中变化 */
  (event: 'update:modelValue', value: string[]): void;
}>();

/** 下拉选项 */
const options = ref<Array<{ label: string; value: string }>>([]);
/** 搜索中 */
const loading = ref(false);
/** 防抖句柄 */
let timer: ReturnType<typeof setTimeout> | null = null;

/** 用户展示名：姓名优先，退化为账号 */
function displayName(user: UserRes): string {
  return user.real_name ? `${user.real_name}（${user.username}）` : user.username;
}

/** 把用户塞进选项表（按 id 去重，保留已选中的那条不被刷掉） */
function mergeOptions(users: UserRes[]): void {
  const map = new Map<string, { label: string; value: string }>();
  for (const option of options.value) {
    map.set(option.value, option);
  }
  for (const user of users) {
    map.set(user.id, { label: displayName(user), value: user.id });
  }
  options.value = Array.from(map.values());
}

/** 关键词搜索（用户名 / 姓名并发，结果合并去重） */
async function search(keyword: string): Promise<void> {
  const trimmed = keyword.trim();
  loading.value = true;
  try {
    const [byName, byRealName] = await Promise.all([
      pageUsers({ page: 1, page_size: 20, username: trimmed || undefined }),
      trimmed ? pageUsers({ page: 1, page_size: 20, real_name: trimmed }) : Promise.resolve(null),
    ]);
    mergeOptions(byName.list);
    if (byRealName) {
      mergeOptions(byRealName.list);
    }
  } catch {
    // 用户列表拉取失败不阻断评论：保持现有选项，输入框仍可用
  } finally {
    loading.value = false;
  }
}

/** 输入关键词：300ms 防抖后再打接口 */
function onSearch(keyword: string): void {
  if (timer) {
    clearTimeout(timer);
  }
  timer = setTimeout(() => {
    void search(keyword);
  }, 300);
}

/** 选中变化（TDesign 多选下发数组，这里强类型化并剔除空值） */
function onChange(value: unknown): void {
  const list = Array.isArray(value) ? value.filter((item): item is string => typeof item === 'string' && item !== '') : [];
  emit('update:modelValue', list);
}

// 首次挂载：预取一页供直接点选
void search('');
</script>

<style scoped lang="less">
.mention-select {
  width: 100%;
}
</style>
