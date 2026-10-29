<template>
  <div class="node-props">
    <!-- ---------- 节点属性 ---------- -->
    <template v-if="node">
      <div class="node-props__head">
        <span class="node-props__title">{{ nodeTypeLabel(node.type) }}节点</span>
        <t-tag size="small" variant="outline">{{ node.key }}</t-tag>
      </div>

      <t-form :label-width="76" label-align="top" class="node-props__form" :disabled="readonly">
        <t-form-item label="名称">
          <t-input
            :value="node.name"
            placeholder="节点名称（≤100 字）"
            :maxlength="100"
            @change="(value: unknown) => emitPatch({ name: String(value ?? '') })"
          />
        </t-form-item>

        <!-- 审批人（审批 / 会签 / 抄送） -->
        <template v-if="needsAssignee">
          <t-form-item label="审批人来源">
            <t-select
              :value="node.assignee_type ?? undefined"
              :options="assigneeKindOptions"
              placeholder="请选择审批人来源"
              @change="onAssigneeTypeChange"
            />
          </t-form-item>

          <t-form-item v-if="node.assignee_type === 'INITIATOR_SUPERIOR'" label="主管层数">
            <t-input-number
              :value="node.superior_levels ?? 1"
              :min="1"
              :max="5"
              theme="normal"
              @change="(value: unknown) => emitPatch({ superior_levels: Number(value) || 1 })"
            />
          </t-form-item>

          <t-form-item v-if="node.assignee_type === 'FORM_VAR'" label="表单变量">
            <t-input
              :value="node.form_var_key ?? ''"
              placeholder="表单字段名，如 task_owner_id"
              @change="(value: unknown) => emitPatch({ form_var_key: String(value ?? '') || null })"
            />
          </t-form-item>

          <!-- 可枚举审批人（USER / ROLE / DEPT_LEADER 落行） -->
          <t-form-item v-if="supportsAssigneeRows" label="指定审批人">
            <div class="node-props__assignees">
              <div v-for="(row, index) in node.assignees" :key="`${row.type}-${index}`" class="node-props__assignee">
                <t-select
                  :value="row.type"
                  :options="rowKindOptions"
                  class="node-props__assignee-type"
                  @change="(value: unknown) => onRowKindChange(index, value)"
                />
                <UserSelect
                  v-if="row.type === 'USER'"
                  :model-value="row.ref_id"
                  class="node-props__assignee-value"
                  @update:model-value="(value: string | null) => onRowRefChange(index, value ?? '')"
                />
                <t-select
                  v-else-if="row.type === 'ROLE'"
                  :value="row.ref_id || undefined"
                  :options="roleOptions"
                  :loading="roleLoading"
                  filterable
                  placeholder="选择角色"
                  class="node-props__assignee-value"
                  @change="(value: unknown) => onRowRefChange(index, String(value ?? ''))"
                />
                <DeptTreeSelect
                  v-else
                  :model-value="row.ref_id"
                  class="node-props__assignee-value"
                  @update:model-value="(value: string | undefined) => onRowRefChange(index, value ?? '')"
                />
                <t-button size="small" variant="text" theme="danger" @click="removeAssignee(index)">
                  <t-icon name="delete" />
                </t-button>
              </div>
              <t-button size="small" variant="outline" @click="addAssignee">
                <template #icon><t-icon name="add" /></template>
                添加审批人
              </t-button>
              <p class="node-props__tip">
                只有「指定人 / 角色 / 部门负责人」需要在这里落行；发起人、主管、表单变量由引擎运行时解析。
              </p>
            </div>
          </t-form-item>

          <t-form-item label="空审批人">
            <t-select
              :value="node.empty_assignee"
              :options="emptyAssigneeOptions"
              @change="(value: unknown) => emitPatch({ empty_assignee: String(value ?? 'SKIP') })"
            />
          </t-form-item>

          <t-form-item label="自审策略">
            <t-select
              :value="node.self_approval"
              :options="selfApprovalOptions"
              @change="(value: unknown) => emitPatch({ self_approval: String(value ?? 'SKIP') })"
            />
          </t-form-item>
        </template>

        <!-- 会签 / 或签 -->
        <template v-if="supportsCountersign">
          <t-form-item label="审批模式">
            <t-select
              :value="node.approval_mode ?? 'ALL'"
              :options="approvalModeOptions"
              @change="onApprovalModeChange"
            />
          </t-form-item>
          <t-form-item v-if="node.approval_mode === 'RATIO'" label="通过比例">
            <t-input-number
              :value="node.approve_ratio ?? 0.6"
              :min="0.1"
              :max="1"
              :step="0.1"
              :decimal-places="2"
              theme="normal"
              @change="(value: unknown) => emitPatch({ approve_ratio: Number(value) || 0.6 })"
            />
            <p class="node-props__tip">(0,1] 之间；0.6 = 60% 的人同意即过。</p>
          </t-form-item>
          <t-form-item label="一票否决">
            <t-switch :value="node.reject_veto" @change="(value: unknown) => emitPatch({ reject_veto: Boolean(value) })" />
          </t-form-item>
        </template>

        <!-- 网关模式 -->
        <template v-if="supportsGateway">
          <t-form-item v-if="node.type === 'PARALLEL'" label="分叉方式">
            <t-select
              :value="node.fork_mode"
              :options="gatewayOptions"
              @change="(value: unknown) => emitPatch({ fork_mode: String(value ?? 'AND') })"
            />
          </t-form-item>
          <t-form-item :label="node.type === 'PARALLEL' ? '汇聚方式' : '汇聚方式'">
            <t-select
              :value="node.join_mode"
              :options="gatewayOptions"
              @change="(value: unknown) => emitPatch({ join_mode: String(value ?? 'AND') })"
            />
          </t-form-item>
          <p class="node-props__tip">
            {{ node.type === 'PARALLEL' ? '并行网关：AND = 全部走完才继续，OR = 任一到达即继续。' : '条件节点的汇聚方式：AND = 所有入边到齐，OR = 任一到达。' }}
          </p>
        </template>

        <!-- 超时策略 -->
        <template v-if="supportsTimeout">
          <t-form-item label="超时小时">
            <t-input-number
              :value="node.timeout_hours ?? undefined"
              :min="1"
              :max="720"
              theme="normal"
              placeholder="不填 = 不限制"
              @change="(value: unknown) => emitPatch({ timeout_hours: toOptionalNumber(value) })"
            />
          </t-form-item>
          <t-form-item v-if="node.timeout_hours" label="超时动作">
            <t-select
              :value="node.timeout_action"
              :options="timeoutActionOptions"
              @change="(value: unknown) => emitPatch({ timeout_action: String(value ?? 'REMIND') })"
            />
          </t-form-item>
          <t-form-item v-if="node.timeout_hours" label="提前提醒">
            <t-input-number
              :value="node.remind_before_hours ?? undefined"
              :min="1"
              :max="72"
              theme="normal"
              placeholder="小时"
              @change="(value: unknown) => emitPatch({ remind_before_hours: toOptionalNumber(value) })"
            />
          </t-form-item>
        </template>

        <t-form-item v-if="supportsAssignee" label="允许机器人">
          <t-switch :value="node.allow_robot" @change="(value: unknown) => emitPatch({ allow_robot: Boolean(value) })" />
        </t-form-item>

        <t-form-item v-if="supportsAssignee" label="通过后抄送">
          <t-switch :value="node.cc_on_approved" @change="(value: unknown) => emitPatch({ cc_on_approved: Boolean(value) })" />
        </t-form-item>
      </t-form>

      <p v-if="readonly" class="node-props__readonly-tip">该版本已发布，属性为只读。</p>
    </template>

    <!-- ---------- 连线属性 ---------- -->
    <template v-else-if="edge">
      <div class="node-props__head">
        <span class="node-props__title">连线</span>
        <t-tag size="small" variant="outline">{{ edge.key }}</t-tag>
      </div>
      <p class="node-props__tip">{{ edge.from }} → {{ edge.to }}</p>

      <t-form :label-width="76" label-align="top" class="node-props__form" :disabled="readonly">
        <t-form-item label="条件标签">
          <t-input
            :value="edge.label ?? ''"
            placeholder="显示在连线上的文案，如 金额 ≥ 1 万"
            :maxlength="60"
            @change="(value: unknown) => emitPatchEdge({ label: String(value ?? '') || null })"
          />
        </t-form-item>
        <t-form-item label="兜底分支">
          <t-switch
            :value="edge.is_default"
            @change="onDefaultEdgeChange"
          />
          <p class="node-props__tip">条件节点的出边必须恰有一条兜底（其他情况都走它）。</p>
        </t-form-item>
        <t-form-item label="优先级">
          <t-input-number
            :value="edge.priority"
            :min="0"
            :max="99"
            theme="normal"
            @change="(value: unknown) => emitPatchEdge({ priority: Number(value) || 0 })"
          />
        </t-form-item>
        <t-form-item label="条件表达式">
          <t-textarea
            v-model="conditionText"
            placeholder='JSON DSL，如 {"all":[{"field":"amount","op":"gte","value":10000}]}'
            :autosize="{ minRows: 3, maxRows: 8 }"
            @blur="commitCondition"
          />
          <p class="node-props__tip">留空表示无条件。表达式是否合法由后端 V11 校验。</p>
        </t-form-item>
      </t-form>

      <p v-if="readonly" class="node-props__readonly-tip">该版本已发布，连线为只读。</p>
    </template>

    <!-- ---------- 空态 ---------- -->
    <div v-else class="node-props__empty">
      <t-icon name="edit-1" size="26px" />
      <p>选中画布上的节点或连线后，在这里编辑属性</p>
    </div>
  </div>
</template>

<script setup lang="ts">
/**
 * 属性面板（架构 §11.7 右栏 / PRD 5.4「按节点类型的字段表」）。
 *
 * # 职责边界
 *
 * 本组件**不直接改 props**：任何字段变化都通过 `patch-node` / `patch-edge` 事件抛给编辑器页面，
 * 由页面压一条 `patch-node` 命令（可撤销）。这样「属性改动」和「拖拽移动」共用同一个撤销栈。
 *
 * # 为什么审批人来源用下拉而不是自由文本
 *
 * 后端 `AssigneeKind::parse` 只认 10 个字面量，自由文本会得到 400 + 65005。
 * 这里把 10 个值都做成选项，其中「指定人 / 角色 / 部门负责人」三种可落行（另配 ref_id 列表）。
 */
import { computed, ref, watch } from 'vue';
import type { FlowAssignee, FlowEdge, FlowNode } from '@/api/business/flow-api';
import { listRoles, type RoleRes } from '@/api/system/role-api';
import UserSelect from '@/views/business/task/components/UserSelect.vue';
import DeptTreeSelect from '@/components/DeptTreeSelect.vue';
import { nodeTypeLabel } from '@/views/business/flow/graph';

/** 组件入参 */
const props = withDefaults(
  defineProps<{
    /** 选中节点（与 `edge` 二者其一） */
    node?: FlowNode | null;
    /** 选中连线 */
    edge?: FlowEdge | null;
    /** 只读（已发布版本） */
    readonly?: boolean;
  }>(),
  { node: null, edge: null, readonly: false },
);

/** 组件事件 */
const emit = defineEmits<{
  /** 改节点属性（只带被改的字段） */
  (event: 'patch-node', payload: { key: string; patch: Partial<FlowNode> }): void;
  /** 改连线属性 */
  (event: 'patch-edge', payload: { key: string; patch: Partial<FlowEdge> }): void;
}>();

/** 审批人来源选项（顺序：最常用在上） */
const assigneeKindOptions = [
  { label: '发起人主管', value: 'INITIATOR_SUPERIOR' },
  { label: '发起人本人', value: 'INITIATOR' },
  { label: '指定人', value: 'USER' },
  { label: '指定角色', value: 'ROLE' },
  { label: '部门负责人', value: 'DEPT_LEADER' },
  { label: '任务负责人', value: 'TASK_OWNER' },
  { label: '任务创建人', value: 'TASK_CREATOR' },
  { label: '任务分派人', value: 'TASK_ASSIGNER' },
  { label: '表单字段里的人', value: 'FORM_VAR' },
  { label: '管理员', value: 'ADMIN' },
];

/** 可落行的审批人类型 */
const rowKindOptions = [
  { label: '指定人', value: 'USER' },
  { label: '指定角色', value: 'ROLE' },
  { label: '部门负责人', value: 'DEPT_LEADER' },
];

/** 空审批人策略 */
const emptyAssigneeOptions = [
  { label: '跳过该节点', value: 'SKIP' },
  { label: '报错（流程卡住）', value: 'ERROR' },
  { label: '兜底给管理员', value: 'FALLBACK_ADMIN' },
];

/** 自审策略 */
const selfApprovalOptions = [
  { label: '跳过（本人不审）', value: 'SKIP' },
  { label: '允许自审', value: 'ALLOW' },
  { label: '禁止（报错）', value: 'FORBID' },
];

/** 审批模式 */
const approvalModeOptions = [
  { label: '全部同意（会签）', value: 'ALL' },
  { label: '按比例通过', value: 'RATIO' },
  { label: '任一人同意（或签）', value: 'ANY' },
];

/** 网关模式 */
const gatewayOptions = [
  { label: 'AND（全部）', value: 'AND' },
  { label: 'OR（任一）', value: 'OR' },
];

/** 超时动作 */
const timeoutActionOptions = [
  { label: '仅提醒', value: 'REMIND' },
  { label: '自动通过', value: 'AUTO_APPROVE' },
  { label: '自动驳回', value: 'AUTO_REJECT' },
  { label: '升级给上级', value: 'ESCALATE' },
];

/** 是否审批类节点（需要审批人） */
const needsAssignee = computed(() => {
  const type = props.node?.type;
  return type === 'APPROVAL' || type === 'COUNTERSIGN' || type === 'CC';
});

/** 是否支持会签配置 */
const supportsCountersign = computed(() => {
  const type = props.node?.type;
  return type === 'APPROVAL' || type === 'COUNTERSIGN';
});

/** 是否支持网关配置 */
const supportsGateway = computed(() => props.node?.type === 'PARALLEL' || props.node?.type === 'CONDITION');

/** 是否支持超时配置 */
const supportsTimeout = computed(() => props.node?.type === 'APPROVAL' || props.node?.type === 'COUNTERSIGN');

/** 「允许机器人 / 通过后抄送」只对审批类有意义 */
const supportsAssignee = computed(() => props.node?.type === 'APPROVAL' || props.node?.type === 'COUNTERSIGN');

/** 是否可枚举审批人（三种可落行的来源） */
const supportsAssigneeRows = computed(() =>
  ['USER', 'ROLE', 'DEPT_LEADER'].includes(props.node?.assignee_type ?? ''),
);

/** 角色选项（只加载一次，首次点开属性面板时懒加载） */
const roleOptions = ref<Array<{ label: string; value: string }>>([]);
const roleLoading = ref(false);
let roleLoaded = false;

/** 角色列表（失败就留空，不阻断属性编辑） */
async function loadRoles(): Promise<void> {
  if (roleLoaded || roleLoading.value) {
    return;
  }
  roleLoading.value = true;
  try {
    const list: RoleRes[] = await listRoles();
    roleOptions.value = (list ?? []).map((role) => ({ label: role.role_name, value: role.id }));
    roleLoaded = true;
  } catch {
    roleOptions.value = [];
  } finally {
    roleLoading.value = false;
  }
}

/** 连线条件表达式的本地文本（失焦时才提交，避免边打字边报错） */
const conditionText = ref('');

watch(
  () => props.edge?.key,
  () => {
    const condition = props.edge?.condition;
    conditionText.value = condition ? JSON.stringify(condition, null, 2) : '';
  },
  { immediate: true },
);

watch(
  () => props.node?.assignee_type,
  (value) => {
    if (value === 'ROLE') {
      void loadRoles();
    }
  },
  { immediate: true },
);

/** 抛节点属性变更 */
function emitPatch(patch: Partial<FlowNode>): void {
  if (!props.node || props.readonly) {
    return;
  }
  emit('patch-node', { key: props.node.key, patch });
}

/** 抛连线属性变更 */
function emitPatchEdge(patch: Partial<FlowEdge>): void {
  if (!props.edge || props.readonly) {
    return;
  }
  emit('patch-edge', { key: props.edge.key, patch });
}

/** 切换审批人来源：切到可落行来源时顺手补一行，切走时清空落行数据 */
function onAssigneeTypeChange(value: unknown): void {
  const kind = String(value ?? '');
  const patch: Partial<FlowNode> = { assignee_type: kind || null };
  if (['USER', 'ROLE', 'DEPT_LEADER'].includes(kind)) {
    if (!props.node || props.node.assignees.length === 0) {
      patch.assignees = [{ type: kind as FlowAssignee['type'], ref_id: '', sort_order: 0 }];
    }
    if (kind === 'ROLE') {
      void loadRoles();
    }
  } else {
    patch.assignees = [];
  }
  emitPatch(patch);
}

/** 切换审批模式：切到 RATIO 时补一个默认比例（后端保存期会拦「按比例但没有比例」） */
function onApprovalModeChange(value: unknown): void {
  const mode = String(value ?? 'ALL');
  const patch: Partial<FlowNode> = { approval_mode: mode };
  if (mode === 'RATIO' && !props.node?.approve_ratio) {
    patch.approve_ratio = 0.6;
  }
  if (mode !== 'RATIO') {
    patch.approve_ratio = null;
  }
  emitPatch(patch);
}

/** 改某一行的来源类型 */
function onRowKindChange(index: number, value: unknown): void {
  const rows = [...(props.node?.assignees ?? [])];
  const kind = String(value ?? 'USER') as FlowAssignee['type'];
  if (!rows[index]) {
    return;
  }
  rows[index] = { ...rows[index], type: kind, ref_id: '' };
  if (kind === 'ROLE') {
    void loadRoles();
  }
  emitPatch({ assignees: rows.map((row, order) => ({ ...row, sort_order: order })) });
}

/** 改某一行的目标 id */
function onRowRefChange(index: number, refId: string): void {
  const rows = [...(props.node?.assignees ?? [])];
  if (!rows[index]) {
    return;
  }
  rows[index] = { ...rows[index], ref_id: refId };
  emitPatch({ assignees: rows.map((row, order) => ({ ...row, sort_order: order })) });
}

/** 追加一行审批人 */
function addAssignee(): void {
  const rows = [...(props.node?.assignees ?? [])];
  const kind = (props.node?.assignee_type ?? 'USER') as FlowAssignee['type'];
  rows.push({ type: ['USER', 'ROLE', 'DEPT_LEADER'].includes(kind) ? kind : 'USER', ref_id: '', sort_order: rows.length });
  emitPatch({ assignees: rows });
}

/** 删除一行审批人 */
function removeAssignee(index: number): void {
  const rows = (props.node?.assignees ?? []).filter((_, order) => order !== index);
  emitPatch({ assignees: rows.map((row, order) => ({ ...row, sort_order: order })) });
}

/** 把「可选数字输入框」的值收窄成 number | null（空 = 不填） */
function toOptionalNumber(value: unknown): number | null {
  if (value === undefined || value === null || value === '') {
    return null;
  }
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

/** 切「兜底分支」：打开时顺手把标签写成「其他情况」 */
function onDefaultEdgeChange(value: unknown): void {
  const isDefault = Boolean(value);
  emitPatchEdge({ is_default: isDefault, label: isDefault ? '其他情况' : (props.edge?.label ?? null) });
}

/** 提交条件表达式（空串 → null；非法 JSON → toast 提示并保留文本） */
function commitCondition(): void {
  const text = conditionText.value.trim();
  if (!text) {
    emitPatchEdge({ condition: null });
    return;
  }
  try {
    const parsed: unknown = JSON.parse(text);
    emitPatchEdge({ condition: parsed });
  } catch {
    // 非法 JSON 不提交：保留用户输入，由后端 V11 提示（这里不强改用户写的东西）
  }
}
</script>

<style scoped lang="less">
.node-props {
  height: 100%;
  padding: 12px 14px 24px;
  overflow-y: auto;

  &__head {
    display: flex;
    gap: 8px;
    align-items: center;
    justify-content: space-between;
    padding-bottom: 10px;
    margin-bottom: 12px;
    border-bottom: 1px solid var(--td-component-stroke);
  }

  &__title {
    font-size: 14px;
    font-weight: 600;
    color: var(--td-text-color-primary);
  }

  &__form {
    :deep(.t-form__item) {
      margin-bottom: 12px;
    }
  }

  &__tip {
    margin-top: 4px;
    font-size: 12px;
    line-height: 1.5;
    color: var(--td-text-color-placeholder);
  }

  &__assignees {
    display: flex;
    flex-direction: column;
    gap: 8px;
    width: 100%;
  }

  &__assignee {
    display: flex;
    gap: 6px;
    align-items: center;
  }

  &__assignee-type {
    width: 104px;
    flex: none;
  }

  &__assignee-value {
    min-width: 0;
    flex: 1;
  }

  &__readonly-tip {
    margin-top: 12px;
    font-size: 12px;
    color: var(--td-warning-color);
  }

  &__empty {
    display: flex;
    flex-direction: column;
    gap: 8px;
    align-items: center;
    justify-content: center;
    height: 100%;
    padding: 40px 20px;
    font-size: 13px;
    color: var(--td-text-color-placeholder);
    text-align: center;
  }
}
</style>
