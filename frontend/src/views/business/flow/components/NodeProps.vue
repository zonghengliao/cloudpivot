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

        <!-- ===== 通知配置（仅 NOTIFY_NODE，P-FLOW-2）=====
             紧跟在「名称」之后：这个节点的主体就是「发给谁 + 发什么」，
             触发时机也在里面，先配它再配超时（ON_TIMEOUT 依赖超时小时）。 -->
        <NotifyNodeConfig
          v-if="isNotifyNode(node.type)"
          :node="node"
          :biz-type="bizType"
          :form-fields="formFields"
          :readonly="readonly"
          @patch="emitPatch"
        />

        <!-- ===== 任务配置（仅 TASK_NODE，P-FLOW-1C）=====
             放在「基础信息」之后、「负责人」之前：先配「这一步要干什么活」，再配「谁来干」。
             组件的根是片段，`t-form-item` 直接落在这个 `<t-form>` 里（共用 label-width 与 disabled）。 -->
        <TaskNodeConfig
          v-if="isTaskNode(node.type)"
          :node="node"
          :biz-type="bizType"
          :form-fields="formFields"
          :readonly="readonly"
          @patch="emitPatch"
        />

        <!-- 审批人（审批 / 会签 / 抄送 / 任务负责人） -->
        <template v-if="needsAssignee">
          <t-form-item :label="assigneeSourceLabel">
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
            <p v-if="isTaskNode(node.type)" class="node-props__tip">
              任务节点上这是「解析不出负责人时怎么办」：默认应选「报错」——
              选「跳过」等于这一步的活没人做且没有记录。
            </p>
          </t-form-item>

          <!-- 自审策略只对审批成立；任务节点上配了也不读（设计 §2.4），所以直接不显示 -->
          <t-form-item v-if="!isTaskNode(node.type)" label="自审策略">
            <t-select
              :value="node.self_approval"
              :options="selfApprovalOptions"
              @change="(value: unknown) => emitPatch({ self_approval: String(value ?? 'SKIP') })"
            />
          </t-form-item>

          <!-- 兜底审批人（P3-03 / ARCH-8 场景 C）：升级链走完仍无人时由它接管 -->
          <t-form-item v-if="!isTaskNode(node.type)" label="兜底审批人">
            <div class="node-props__fallback">
              <t-select
                :value="fallbackMode"
                :options="fallbackModeOptions"
                @change="onFallbackModeChange"
              />
              <UserSelect
                v-if="fallbackMode === 'USER'"
                :model-value="node.fallback_user_id ?? null"
                class="node-props__fallback-value"
                @update:model-value="(value: string | null) => onFallbackUserChange(value)"
              />
              <t-select
                v-else-if="fallbackMode === 'ROLE'"
                :value="node.fallback_role_id ?? undefined"
                :options="roleOptions"
                :loading="roleLoading"
                filterable
                placeholder="选择兜底角色"
                class="node-props__fallback-value"
                @change="(value: unknown) => emitPatch({ fallback_role_id: String(value ?? '') || null })"
              />
              <p class="node-props__tip">
                升级链走完仍无人时由它接手（要多人兜底就选「兜底角色」）。
                兜底人同样要过停用 / 机器人 / 自审过滤 —— 兜底不是免审，配成发起人会被拒绝。
              </p>
            </div>
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

        <t-form-item v-if="supportsAssignee && !isTaskNode(node.type)" label="通过后抄送">
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
          <!-- 说明文案（留空=无条件 / 上限 / 高级原始表达式）已收进 ConditionTree 底部的说明区：
               t-form-item 的内容列在窄面板里只有 200px 上下，放在这里会被压成左右两栏、
               中文一行只剩几个字（用户原话：「积压的不方便看」）。 -->
          <div class="node-props__rule" :class="{ 'node-props__rule--error': ruleReport && !ruleReport.ok }">
            <ConditionTree
              :model-value="edge.condition ?? null"
              :readonly="readonly"
              @update:model-value="onConditionChange"
            >
              <!-- 规则预览（P1-B3）：人话翻译 + 三条闸的计数，与红框同一份体检结果。
                   走 ConditionTree 的默认插槽 → 渲染在条件树与底部说明区之间。 -->
              <RulePreview
                :condition="edge.condition ?? null"
                :targets="thenTargets"
                :edge="edge"
                :node="targetNode"
                :report="ruleReport"
              />
            </ConditionTree>
          </div>
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
import ConditionTree from '@/views/business/flow/components/ConditionTree.vue';
import RulePreview from '@/views/business/flow/components/RulePreview.vue';
import NotifyNodeConfig from '@/views/business/flow/components/NotifyNodeConfig.vue';
import TaskNodeConfig from '@/views/business/flow/components/TaskNodeConfig.vue';
import {
  assigneeKindOptionsOf,
  capabilitiesOf,
  isNotifyNode,
  isTaskNode,
  timeoutActionOptionsOf,
} from '@/views/business/flow/components/flow-node-caps';
import { thenTargetsFromNode } from '@/views/business/flow/components/rule-preview';
import { validateRule, type RuleReport } from '@/views/business/flow/components/rule-validation';
import { nodeTypeLabel } from '@/views/business/flow/graph';

/** 组件入参 */
const props = withDefaults(
  defineProps<{
    /** 选中节点（与 `edge` 二者其一） */
    node?: FlowNode | null;
    /** 选中连线 */
    edge?: FlowEdge | null;
    /** 连线的**目标节点**（预览 THEN 段 / 校验 then 目标用；`null` = 没给） */
    targetNode?: FlowNode | null;
    /** 只读（已发布版本） */
    readonly?: boolean;
    /** 本流程业务类型（P-FLOW-1C：`task.*` 变量与指派来源灰化都要它） */
    bizType?: string | null;
    /** 表单字段候选（来自历史实例 `form_data`，可能为空 = 只允许手输） */
    formFields?: string[];
  }>(),
  { node: null, edge: null, targetNode: null, readonly: false, bizType: null, formFields: () => [] },
);

/** 组件事件 */
const emit = defineEmits<{
  /** 改节点属性（只带被改的字段） */
  (event: 'patch-node', payload: { key: string; patch: Partial<FlowNode> }): void;
  /** 改连线属性 */
  (event: 'patch-edge', payload: { key: string; patch: Partial<FlowEdge> }): void;
}>();

/**
 * 能力表（**唯一的类型分支来源**，P-FLOW-1C）。
 *
 * 原来这里有 4 个 `supportsXxx` computed 各自写 `type === 'APPROVAL' || …`，
 * 最长的那个是三段 `||`。加 `TASK_NODE` / `NOTIFY_NODE` 就会变成第 5、6 处 ——
 * 现在统一读 `flow-node-caps.ts` 的表（纯数据，可单测）。
 */
const caps = computed(() => capabilitiesOf(props.node?.type));

/** 审批人来源选项（`TASK_NODE` 上灰掉三个「任务来源」，因为它们依赖 TASK_ACCEPT） */
const assigneeKindOptions = computed(() => assigneeKindOptionsOf(props.node?.type, props.bizType));

/** 超时动作选项（`TASK_NODE` 上灰掉「自动通过 / 自动驳回」：活没干完不能算干完） */
const timeoutActionOptions = computed(() => timeoutActionOptionsOf(props.node?.type));

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

/**
 * 兜底方式（P3-03）：`''` 不兜底 / `'USER'` 指定人 / `'ROLE'` 指定角色。
 *
 * 后端两列**二选一**（DB 有 CHECK、发布期 R22 再挡一次），所以编辑器只暴露一个模式下拉，
 * 不给「两个都填」的机会 —— 那种配置只能换来一条 400/65042。
 */
const fallbackMode = computed(() => {
  if (props.node?.fallback_user_id) return 'USER';
  if (props.node?.fallback_role_id) return 'ROLE';
  return '';
});

/** 兜底方式选项 */
const fallbackModeOptions = [
  { label: '不兜底', value: '' },
  { label: '指定兜底人', value: 'USER' },
  { label: '指定兜底角色（可多人）', value: 'ROLE' },
];

/** 网关模式 */
const gatewayOptions = [
  { label: 'AND（全部）', value: 'AND' },
  { label: 'OR（任一）', value: 'OR' },
];

/** 是否审批类节点（需要审批人）—— 全部来自能力表，不再逐个 `type === … || …` */
const needsAssignee = computed(() => caps.value.canHaveAssignee);

/** 是否支持会签配置 */
const supportsCountersign = computed(() => props.node?.type === 'APPROVAL' || props.node?.type === 'COUNTERSIGN');

/** 是否支持网关配置 */
const supportsGateway = computed(() => props.node?.type === 'PARALLEL' || props.node?.type === 'CONDITION');

/** 是否支持超时配置 */
const supportsTimeout = computed(() => caps.value.canHaveTimeout);

/** 「允许机器人 / 通过后抄送」只对审批类有意义 */
const supportsAssignee = computed(() => caps.value.canHaveAssignee);

/** 「审批人来源」标签：任务节点上是「负责人来源」（术语锁定，决策 4） */
const assigneeSourceLabel = computed(() => (isTaskNode(props.node?.type) ? '负责人来源' : '审批人来源'));

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

/** 条件表达式交给 `ConditionTree.vue`（可视化规则树），这里只做一次「整树 → 一条边命令」的转发 */
function onConditionChange(value: unknown): void {
  emitPatchEdge({ condition: value });
}

/**
 * THEN 段的目标（P1-B3）：连线的目标节点 → `then` 目标数组。
 *
 * `undefined` = 拿不到目标节点（不展示 THEN 段），`[]` = 节点没配审批人（立刻红）。
 */
const thenTargets = computed(() => thenTargetsFromNode(props.targetNode));

/**
 * 实时校验结果（P1-B3 三条闸）：与 `RulePreview.vue` 共用同一份，
 * 这里只拿 `ok` 去给「条件表达式」整块加红框 —— 包括 `ConditionTree` 自己不判的
 * 「规则 ≤ 32」「then 目标为空」两条。
 */
const ruleReport = computed<RuleReport | null>(() => {
  if (!props.edge) return null;
  return validateRule({
    condition: props.edge.condition,
    targets: thenTargets.value,
    edge: props.edge,
    node: props.targetNode,
  });
});

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

/**
 * 切换兜底方式（P3-03）：**二选一**，切走时把另一种清空。
 *
 * 不清空的后果是「两个都填」——后端保存期 400 / DB CHECK 报英文 / 发布期 R22 三处都会炸，
 * 而用户只是换了个下拉。
 */
function onFallbackModeChange(value: unknown): void {
  const mode = String(value ?? '');
  if (mode === 'ROLE') {
    void loadRoles();
  }
  emitPatch({
    fallback_user_id: mode === 'USER' ? (props.node?.fallback_user_id ?? null) : null,
    fallback_role_id: mode === 'ROLE' ? (props.node?.fallback_role_id ?? null) : null,
  });
}

/** 选兜底人（`UserSelect` 清空时给 `null` → 后端落 NULL = 不兜底） */
function onFallbackUserChange(value: string | null): void {
  emitPatch({ fallback_user_id: value ?? null });
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

  /* 规则区（条件树 + 预览）：校验不通过时整块加一圈红，含「规则 ≤ 32」「then 目标为空」两条
     ConditionTree 自己不判的闸（它的红框只管树内部） */
  &__rule {
    width: 100%;
    padding: 2px;
    border-radius: var(--td-radius-default);

    &--error {
      box-shadow: 0 0 0 1px var(--td-error-color);
    }
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

  /* 兜底审批人（P3-03）：模式下拉 + 目标选择器上下排（两者是「二选一」的两段） */
  &__fallback {
    display: flex;
    flex-direction: column;
    gap: 8px;
    width: 100%;
  }

  &__fallback-value {
    width: 100%;
  }

  &__readonly-tip {
    margin-top: 12px;
    font-size: 12px;
    color: var(--td-warning-color);
  }

  /* 通知节点的只读占位（P-FLOW-2 起有正式表单，这两个类名仅为兼容旧样式保留） */
  &__notify {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 10px 12px;
    margin: 0 0 12px;
    background: var(--td-bg-color-container-hover);
    border: 1px dashed var(--td-component-stroke);
    border-radius: var(--td-radius-default);
  }

  &__notify-title {
    display: inline-flex;
    gap: 6px;
    align-items: center;
    font-size: 13px;
    font-weight: 600;
    color: var(--td-text-color-primary);
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
