<template>
  <div class="then-editor" :class="{ 'then-editor--error': issues.length > 0 }">
    <!-- 头部：标题 + 计数 + 口径说明 -->
    <div class="then-editor__head">
      <span class="then-editor__title">命中后由谁审批</span>
      <span
        class="then-editor__meter"
        :class="{ 'then-editor__meter--limit': targets.length > MAX_TARGETS }"
      >
        审批人 {{ targets.length }}/{{ MAX_TARGETS }}
      </span>
      <span class="then-editor__spacer" />
      <span class="then-editor__hint">多个审批人是「并集」（都能收到待办），不是会签</span>
    </div>

    <!-- 只读：一行标签汇总，不渲染任何可编辑控件 -->
    <div v-if="readonly" class="then-editor__readonly">
      <p v-if="targets.length === 0" class="then-editor__empty-text">未配置审批人。</p>
      <t-tag v-for="(target, index) in targets" :key="index" size="small" variant="light">
        {{ describeTarget(target) }}
      </t-tag>
    </div>

    <!-- 空态 -->
    <div v-else-if="targets.length === 0" class="then-editor__empty">
      <p class="then-editor__empty-text">
        还没有审批人 —— 条件命中后无人可派，发布时会被拒（65016）。至少加 1 个。
      </p>
      <t-button size="small" variant="outline" @click="addTarget">
        <template #icon><t-icon name="add" /></template>
        添加审批人
      </t-button>
    </div>

    <!-- 逐行编辑 -->
    <template v-else>
      <div
        v-for="(target, index) in targets"
        :key="index"
        class="then-editor__row"
        :class="{ 'then-editor__row--error': rowError(index) !== null }"
      >
        <!-- 来源 -->
        <t-select
          :value="target.type || undefined"
          class="then-editor__type"
          placeholder="选择来源"
          @change="(value: unknown) => onTypeChange(index, value)"
        >
          <t-option
            v-for="option in SOURCE_OPTIONS"
            :key="option.value"
            :value="option.value"
            :label="option.label"
            :disabled="option.disabled"
          >
            <span class="then-editor__option">
              <span class="then-editor__option-label" :class="{ 'is-disabled': option.disabled }">
                {{ option.label }}
              </span>
              <t-tag v-if="option.reserved" size="small" variant="light" theme="default">P2 支持</t-tag>
              <span v-if="option.hint" class="then-editor__option-hint">{{ option.hint }}</span>
            </span>
          </t-option>
        </t-select>

        <!-- 值：按来源换控件 -->
        <div class="then-editor__value">
          <!-- USER：可多选（= 卡面的 STATIC，写死 uuid 列表），与评论 @ 用的是同一个多选组件 -->
          <MentionSelect
            v-if="inputKind(target.type) === 'users'"
            :model-value="target.userIds"
            :placeholder="`搜索并选择（最多 ${MAX_USER_IDS} 人）`"
            @update:model-value="(value: string[]) => onIdsChange(index, value)"
          />

          <!-- ROLE：按 role_code 存（D-E4），标签里同时给名称与编码 -->
          <t-select
            v-else-if="inputKind(target.type) === 'roles'"
            :value="target.roleCodes"
            :options="roleSelectOptions"
            :loading="roleLoading"
            multiple
            filterable
            clearable
            placeholder="选择角色（按编码存，跨环境有效）"
            @change="(value: unknown) => onCodesChange(index, value)"
          />

          <!-- DEPT_LEADER：留空 = 发起人所在部门 -->
          <DeptTreeSelect
            v-else-if="inputKind(target.type) === 'dept'"
            :model-value="target.deptIds[0] ?? null"
            placeholder="不选 = 发起人所在部门的负责人"
            @update:model-value="(value: string | undefined) => onDeptChange(index, value)"
          />

          <!-- INITIATOR_SUPERIOR：上溯层数 -->
          <div v-else-if="inputKind(target.type) === 'levels'" class="then-editor__levels">
            <span class="then-editor__field-label">上溯</span>
            <t-input-number
              :value="target.levels"
              :min="1"
              :max="MAX_SUPERIOR_LEVELS"
              theme="normal"
              class="then-editor__levels-input"
              @change="(value: unknown) => onLevelsChange(index, value)"
            />
            <span class="then-editor__field-label">级上级</span>
          </div>

          <!-- FORM_VAR：表单字段名，值是 uuid -->
          <t-input
            v-else-if="inputKind(target.type) === 'formVar'"
            :value="target.formVarKey"
            placeholder="表单字段名（如 owner_id），字段里存的是 uuid"
            @change="(value: unknown) => onFormVarChange(index, value)"
          />

          <!-- 无需输入：只说明运行时取谁 -->
          <span v-else class="then-editor__auto" :class="{ 'is-disabled': isReserved(target.type) }">
            {{ autoHint(target.type) }}
          </span>
        </div>

        <t-button
          size="small"
          variant="text"
          theme="danger"
          class="then-editor__remove"
          title="删除这个审批人"
          @click="removeTarget(index)"
        >
          <t-icon name="close" />
        </t-button>
      </div>

      <div class="then-editor__row-actions">
        <t-button
          size="small"
          variant="outline"
          :disabled="targets.length >= MAX_TARGETS"
          @click="addTarget"
        >
          <template #icon><t-icon name="add" /></template>
          添加审批人
        </t-button>
        <span v-if="targets.length >= MAX_TARGETS" class="then-editor__hint">
          单条规则最多 {{ MAX_TARGETS }} 个审批人（后端上限）
        </span>
      </div>
    </template>

    <!-- 校验问题汇总（红字，行内也标红） -->
    <ul v-if="issues.length > 0" class="then-editor__issues">
      <li v-for="(issue, index) in issues.slice(0, 4)" :key="index" class="then-editor__issue">
        {{ issue.message }}
      </li>
      <li v-if="issues.length > 4" class="then-editor__issue">… 还有 {{ issues.length - 4 }} 条</li>
    </ul>

    <!-- 保留词提示：让用户知道「有这个能力，本期灰着」 -->
    <div class="then-editor__reserved">
      <t-tag size="small" variant="light" theme="default">P2 支持</t-tag>
      <span class="then-editor__reserved-text">
        「上一节点审批人」（PREV_APPROVER）本期不支持，下拉里灰显；现在选它只会在发布时被拒（65033）。
      </span>
    </div>
  </div>
</template>

<script setup lang="ts">
/**
 * then 目标选择器（批次 D · P1-B2）—— 条件命中后「谁来审」。
 *
 * # 双向绑定
 *
 * `modelValue` 就是后端规则里的 `then`（`unknown`，可能是 `null` / 老版本 JSON）。
 * 进来过 `fromThenJson()` 反解成编辑期数组，出去过 `toThenJson()` 生成**键名固定**的
 * JSON 数组，与 `expression.rs::parse_target` 逐字兼容（键名/上限/来源枚举见 `then-target.ts`）。
 *
 * 改动收敛成**一次** `update:modelValue`（与 `ConditionTree.vue` 同口径）：
 * 属性面板把每条命令压进撤销栈，一次编辑 = 一条可撤销命令。
 *
 * # 10 种来源 ↔ 卡面口径
 *
 * 卡面写的 `SUPERVISOR / INITIATOR_DEPT / STATIC` 在库与后端都没有；真实落地是
 * `INITIATOR_SUPERIOR（带 levels）/ DEPT_LEADER（留空=发起人部门）/ USER（多选 uuid）`。
 * 语义一一对应，细节见 `then-target.ts` 头注释的对照表。
 *
 * # 两条硬红线
 *
 * - **ROLE 存 `role_code`**（D-E4）：`role_id` 是 `gen_random_uuid()`，跨环境导出就会指错角色。
 * - **PREV_APPROVER 不可选**（D-E8）：后端解析期硬拒（65033），前端不能让它「存得进去、
 *   跑起来才炸」。下拉里灰显 + 「P2 支持」标签；旧数据里真有则红框提示，且不写回产出 JSON。
 *
 * # 挂载位置
 *
 * 规则列表（`{id, when, then}` 的 if/else 链）是 P1-B3 的活；本组件对 `then` 数组本身
 * 自给自足，P1-B3 的规则行直接 `<ThenTargetEditor v-model="rule.then" />` 即可。
 */
import { ref, watch } from 'vue';
import { listRoles, type RoleRes } from '@/api/system/role-api';
import DeptTreeSelect from '@/components/DeptTreeSelect.vue';
import MentionSelect from '@/views/business/task/components/MentionSelect.vue';
import { semanticKey } from './condition-tree';
import {
  autoHint,
  createTarget,
  describeTarget,
  fromThenJson,
  inputKind,
  isReserved,
  issueByIndex,
  MAX_SUPERIOR_LEVELS,
  MAX_TARGETS,
  MAX_USER_IDS,
  SOURCE_OPTIONS,
  toThenJson,
  validateTargets,
  type ThenTarget,
  type TargetIssue,
} from './then-target';

/** 组件入参 */
const props = withDefaults(
  defineProps<{
    /** 后端 `then` 原文（null / undefined = 还没配） */
    modelValue?: unknown;
    /** 只读（已发布版本） */
    readonly?: boolean;
  }>(),
  { readonly: false },
);

/** 组件事件：整组 → JSON 数组 */
const emit = defineEmits<{
  (event: 'update:modelValue', value: Array<Record<string, unknown>>): void;
}>();

/** 编辑期唯一真源 */
const targets = ref<ThenTarget[]>([]);
/** 反解旧数据时发现的问题（脏数据不崩，只提示） */
const parseIssues = ref<TargetIssue[]>([]);

/** 角色选项（懒加载一次；拉不到就留空，不阻断编辑） */
const roleOptions = ref<Array<{ label: string; value: string }>>([]);
const roleLoading = ref(false);
let roleLoaded = false;

/**
 * 加载角色下拉。
 *
 * **value 用 `role_code` 而不是 `id`**（D-E4）：`id` 由 `gen_random_uuid()` 生成，
 * 流程定义导出到另一个环境后 id 全变，按 id 配的规则会指向不存在的角色。
 */
async function loadRoles(): Promise<void> {
  if (roleLoaded || roleLoading.value) return;
  roleLoading.value = true;
  try {
    const list: RoleRes[] = await listRoles();
    roleOptions.value = (list ?? []).map((role) => ({
      label: `${role.role_name}（${role.role_code}）`,
      value: role.role_code,
    }));
    roleLoaded = true;
  } catch {
    roleOptions.value = [];
  } finally {
    roleLoading.value = false;
  }
}

/**
 * 角色下拉选项：真实角色表 + 旧数据里出现但表里没有的编码。
 *
 * 第二类必须补一条占位选项，否则 `t-select` 找不到 label 会显示成裸编码，
 * 用户看不出是「角色被删了」还是「只是没加载出来」。
 */
const roleSelectOptions = computed<Array<{ label: string; value: string }>>(() => {
  const map = new Map<string, { label: string; value: string }>();
  for (const option of roleOptions.value) map.set(option.value, option);
  for (const target of targets.value) {
    if (inputKind(target.type) !== 'roles') continue;
    for (const code of target.roleCodes) {
      if (!map.has(code)) {
        map.set(code, { label: `${code}（未匹配到角色）`, value: code });
      }
    }
  }
  return Array.from(map.values());
});

/** 反解：JSON → 编辑期数组 */
function rebuild(raw: unknown): void {
  const parsed = fromThenJson(raw);
  targets.value = parsed.targets;
  parseIssues.value = parsed.issues;
  if (targets.value.some((target) => inputKind(target.type) === 'roles')) {
    void loadRoles();
  }
}

/**
 * 回灌判断：只有「外部给的值 ≠ 当前数组生成的值」才重建，
 * 否则每次 emit 都会把用户刚敲的输入框重置掉（与 `ConditionTree.vue` 同一处理）。
 */
watch(
  () => props.modelValue,
  (value) => {
    if (semanticKey(value) === semanticKey(toThenJson(targets.value))) return;
    rebuild(value);
  },
  { immediate: true },
);

/** 全组校验（含空数组 / 超 5 个） */
const reportIssues = computed<TargetIssue[]>(() => validateTargets(targets.value));

/**
 * 所有问题（反解问题在前）。
 *
 * **按 `下标 + 文案` 去重**：`fromThenJson` 对脏数据已经报过一次（保留词 / 来源不支持 / 缺值），
 * `validateTargets` 又会基于当前编辑态再报一次 —— 不去重用户会看到同一句话两遍。
 */
const issues = computed<TargetIssue[]>(() => {
  const seen = new Set<string>();
  const out: TargetIssue[] = [];
  for (const issue of [...parseIssues.value, ...reportIssues.value]) {
    const key = `${issue.index}|${issue.message}`;
    if (seen.has(key)) continue;
    seen.add(key);
    out.push(issue);
  }
  return out;
});

/** `下标 → 第一条问题`，行内红框用 */
const rowIssues = computed<Record<number, string>>(() => issueByIndex(issues.value));

/** 第 index 行的第一条问题（没有则 null） */
function rowError(index: number): string | null {
  return rowIssues.value[index] ?? null;
}

/** 提交：整组 → JSON 数组（不合法的项自动跳过，问题已在红字里说明） */
function commit(): void {
  emit('update:modelValue', toThenJson(targets.value));
}

/** 追加一行（默认「指定用户」，最常用） */
function addTarget(): void {
  if (props.readonly || targets.value.length >= MAX_TARGETS) return;
  targets.value.push(createTarget('USER'));
  commit();
}

/** 删除一行 */
function removeTarget(index: number): void {
  if (props.readonly) return;
  targets.value.splice(index, 1);
  parseIssues.value = [];
  commit();
}

/**
 * 切换来源：换成一个干净的目标（沿用旧值会让「按角色」带着上一份 uuid，是脏数据）。
 * 切到 ROLE 时顺手拉角色表。
 */
function onTypeChange(index: number, value: unknown): void {
  const target = targets.value[index];
  if (!target) return;
  const type = String(value ?? '');
  targets.value[index] = createTarget(type);
  parseIssues.value = [];
  if (inputKind(type) === 'roles') {
    void loadRoles();
  }
  commit();
}

/** USER：多选用户 id */
function onIdsChange(index: number, value: string[]): void {
  const target = targets.value[index];
  if (!target) return;
  target.userIds = Array.isArray(value) ? value : [];
  commit();
}

/** ROLE：多选角色编码 */
function onCodesChange(index: number, value: unknown): void {
  const target = targets.value[index];
  if (!target) return;
  target.roleCodes = Array.isArray(value)
    ? value.filter((item): item is string => typeof item === 'string' && item !== '')
    : [];
  commit();
}

/** DEPT_LEADER：单选部门（留空 = 发起人部门）；多部门旧数据保留其余项 */
function onDeptChange(index: number, value: string | undefined): void {
  const target = targets.value[index];
  if (!target) return;
  target.deptIds = value ? [value] : [];
  commit();
}

/** INITIATOR_SUPERIOR：上溯层数（越界交给校验报错，不在这里静默夹取） */
function onLevelsChange(index: number, value: unknown): void {
  const target = targets.value[index];
  if (!target) return;
  const parsed = Number(value);
  target.levels = Number.isFinite(parsed) ? parsed : Number.NaN;
  commit();
}

/** FORM_VAR：表单字段名 */
function onFormVarChange(index: number, value: unknown): void {
  const target = targets.value[index];
  if (!target) return;
  target.formVarKey = String(value ?? '');
  commit();
}
</script>

<style scoped lang="less">
.then-editor {
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

  &__hint {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__readonly {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    padding: 8px 0 2px;
  }

  &__empty {
    padding: 10px 0 4px;
  }

  &__empty-text {
    margin-bottom: 8px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__row {
    display: flex;
    gap: 6px;
    align-items: center;
    padding: 6px 4px;
    margin-top: 6px;
    border: 1px solid transparent;
    border-radius: var(--td-radius-default);
    transition: border-color 0.15s ease, background-color 0.15s ease;

    &:hover {
      background: var(--td-bg-color-container-hover);
    }

    &--error {
      border-color: var(--td-error-color);
      background: var(--td-error-color-1);
    }
  }

  &__type {
    flex: 0 0 150px;
  }

  &__value {
    flex: 1;
    min-width: 0;
  }

  &__remove {
    flex: 0 0 auto;
  }

  &__option {
    display: flex;
    gap: 6px;
    align-items: center;
  }

  &__option-label {
    &.is-disabled {
      color: var(--td-text-color-disabled);
    }
  }

  &__option-hint {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__levels {
    display: flex;
    gap: 6px;
    align-items: center;
  }

  &__levels-input {
    width: 96px;
  }

  &__field-label {
    font-size: 12px;
    color: var(--td-text-color-secondary);
  }

  &__auto {
    font-size: 12px;
    color: var(--td-text-color-placeholder);

    &.is-disabled {
      color: var(--td-error-color);
    }
  }

  &__row-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    align-items: center;
    padding-top: 6px;
  }

  &__issues {
    padding: 6px 8px 6px 22px;
    margin: 6px 0 0;
    list-style: disc;
    border-radius: var(--td-radius-default);
    background: var(--td-error-color-1);
  }

  &__issue {
    font-size: 12px;
    line-height: 1.6;
    color: var(--td-error-color);
  }

  &__reserved {
    display: flex;
    gap: 6px;
    align-items: center;
    padding-top: 6px;
    margin-top: 6px;
    border-top: 1px dashed var(--td-component-stroke);
  }

  &__reserved-text {
    font-size: 12px;
    line-height: 1.5;
    color: var(--td-text-color-placeholder);
  }
}
</style>
