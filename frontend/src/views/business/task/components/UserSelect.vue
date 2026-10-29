<template>
  <t-select
    :value="modelValue ?? undefined"
    :options="options"
    :placeholder="placeholder"
    :clearable="clearable"
    :disabled="disabled"
    :loading="loading"
    filterable
    class="user-select"
    @update:value="onChange"
    @search="onSearch"
  />
</template>

<script setup lang="ts">
/**
 * 用户选择器（负责人 / 筛选条件共用）。
 *
 * # 为什么自己搜而不是一次拉全量
 *
 * `sys_user` 是登记表，`GET /api/system/user/page` 的 `username` 与 `real_name` 是
 * **AND** 关系（后端 model 里各拼一个 `LIKE`），一个搜索框没法同时命中两者，
 * 所以这里并发发两次（用户名 / 姓名各一次）再按 id 去重合并。
 *
 * # 回显
 *
 * 编辑态 `modelValue` 有值但不在当前选项里时，用 `getUser(id)` 补一条选项，
 * 否则下拉会显示成裸 id（t-select 找不到 label 时的默认行为）。
 */
import { ref, watch } from 'vue';
import { getUser, pageUsers, type UserRes } from '@/api/system/user-api';

const props = withDefaults(
  defineProps<{
    /** 选中的用户 id（null / 空串表示未选） */
    modelValue?: string | null;
    /** 占位文案 */
    placeholder?: string;
    /** 是否可清空 */
    clearable?: boolean;
    /** 是否禁用 */
    disabled?: boolean;
  }>(),
  {
    modelValue: null,
    placeholder: '搜索用户名 / 姓名',
    clearable: true,
    disabled: false,
  },
);

const emit = defineEmits<{
  /** 选中值变化 */
  (event: 'update:modelValue', value: string | null): void;
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

/** 把用户塞进选项表（按 id 去重，保持已选中的那条不被刷掉） */
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

/** 关键词搜索（两个字段并发，结果合并去重） */
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
    // 用户列表拉取失败不阻断主流程：保持现有选项，输入框仍可用
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

/** 选中变化 */
function onChange(value: unknown): void {
  emit('update:modelValue', typeof value === 'string' && value ? value : null);
}

/** 回显：外部值有值但不在选项里 → 单查一次补上 */
async function ensureOption(id: string): Promise<void> {
  if (!id || options.value.some((item) => item.value === id)) {
    return;
  }
  try {
    const user = await getUser(id);
    mergeOptions([user]);
  } catch {
    // 用户被删 / 无权限：只显示 id，不阻断页面
  }
}

// 首次打开：预取一页供直接点选
void search('');

watch(
  () => props.modelValue,
  (value) => {
    if (value) {
      void ensureOption(value);
    }
  },
  { immediate: true },
);
</script>

<style scoped lang="less">
.user-select {
  width: 100%;
}
</style>
