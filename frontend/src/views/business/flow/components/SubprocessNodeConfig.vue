<template>
  <!--
    「子流程节点配置」面板（P3-08 · ARCH-5 §4.14）。

    组件的根是**片段**（没有包裹层）：这些 `<t-form-item>` 要落在 `NodeProps.vue` 的
    `<t-form>` 里（那个 form 带着 `label-width` 与 `disabled=readonly`）。
  -->
  <div class="subprocess-node-config__head">
    <span class="subprocess-node-config__title">
      <t-icon name="component" />
      子流程配置
    </span>
    <t-tag size="small" variant="light" theme="warning">父节点阻塞等子单批复</t-tag>
  </div>

  <!-- ---------- 子流程定义（物理列 subprocess_def_id） ---------- -->
  <t-form-item label="调用哪个流程" required-mark>
    <div class="subprocess-node-config__field">
      <t-select
        :value="node?.subprocess_def_id ?? undefined"
        :options="defOptions"
        :loading="loading"
        filterable
        clearable
        placeholder="选择一条已发布的流程"
        :disabled="readonly"
        data-testid="subprocess-def-select"
        @change="onDefChange"
      />
      <p class="subprocess-node-config__tip">
        只能调用「已发布」的流程（发布校验 R27 会拦未发布的，错误码 65045）；也不能指向本流程自己
        （自递归 → 65046）。父子实例靠 <code>parent_instance_id</code> 关联，子单的
        <code>biz_id</code> 恒为空，不会占用父单据的「同时只能一个在跑」唯一键。
      </p>
    </div>
  </t-form-item>

  <!-- ---------- 版本策略 ---------- -->
  <t-form-item label="版本策略">
    <t-radio-group
      :value="cfg.version_policy ?? 'PIN_CURRENT'"
      variant="default-filled"
      :disabled="readonly"
      data-testid="subprocess-version-policy"
      @change="(value: unknown) => patch({ version_policy: value as VersionPolicy })"
    >
      <t-radio-button v-for="item in VERSION_POLICY_OPTIONS" :key="item.value" :value="item.value">
        {{ item.label }}
      </t-radio-button>
    </t-radio-group>
    <p class="subprocess-node-config__tip">{{ versionPolicyHint }}</p>
  </t-form-item>

  <!-- ---------- 失败动作 ---------- -->
  <t-form-item label="子流程没通过">
    <t-radio-group
      :value="cfg.fail_action ?? 'ABORT'"
      variant="default-filled"
      :disabled="readonly"
      data-testid="subprocess-fail-action"
      @change="(value: unknown) => patch({ fail_action: value as FailAction })"
    >
      <t-radio-button v-for="item in FAIL_ACTION_OPTIONS" :key="item.value" :value="item.value">
        {{ item.label }}
      </t-radio-button>
    </t-radio-group>
    <p class="subprocess-node-config__tip">{{ failActionHint }}</p>
  </t-form-item>

  <!-- ---------- 父侧提示任务 ---------- -->
  <t-form-item label="生成提示任务">
    <t-switch
      :value="Boolean(cfg.spawn_task)"
      :disabled="readonly"
      data-testid="subprocess-spawn-task"
      @change="(value: unknown) => patch({ spawn_task: Boolean(value) })"
    />
    <p class="subprocess-node-config__tip">
      打开后，父流程会多一条「子流程运行中：节点名」的工作任务（负责人 = 父单发起人），
      子流程结算时自动完成 / 取消 —— 让父单的人看得见「子流程在跑」。
    </p>
  </t-form-item>

  <!-- ---------- 父 → 子 入参映射 ---------- -->
  <t-form-item label="传给子流程">
    <div class="subprocess-node-config__mapping" data-testid="subprocess-param-mapping">
      <div v-for="(row, index) in paramRows" :key="`param-${index}`" class="subprocess-node-config__row">
        <t-input
          :value="row.key"
          placeholder="子流程字段键"
          :disabled="readonly"
          @change="(value: unknown) => onRow('param_mapping', index, { key: String(value ?? '') })"
        />
        <span class="subprocess-node-config__arrow">←</span>
        <t-input
          :value="row.value"
          placeholder="父表单路径（如 expense.amount）"
          :disabled="readonly"
          @change="(value: unknown) => onRow('param_mapping', index, { value: String(value ?? '') })"
        />
        <t-button v-if="!readonly" size="small" variant="text" theme="danger" @click="removeRow('param_mapping', index)">
          <t-icon name="delete" />
        </t-button>
      </div>
      <t-button v-if="!readonly" size="small" variant="outline" @click="addRow('param_mapping')">
        <t-icon name="add" /> 加一条
      </t-button>
      <p class="subprocess-node-config__tip">
        左边是**子流程的字段键**，右边是父表单里的路径；父表单取不到该路径时按「缺值」处理（不写 null）。
      </p>
    </div>
  </t-form-item>

  <!-- ---------- 子 → 父 结果回写 ---------- -->
  <t-form-item label="回写给父流程">
    <div class="subprocess-node-config__mapping" data-testid="subprocess-result-mapping">
      <div v-for="(row, index) in resultRows" :key="`result-${index}`" class="subprocess-node-config__row">
        <t-input
          :value="row.key"
          placeholder="父表单路径"
          :disabled="readonly"
          @change="(value: unknown) => onRow('result_mapping', index, { key: String(value ?? '') })"
        />
        <span class="subprocess-node-config__arrow">←</span>
        <t-input
          :value="row.value"
          placeholder="子表单路径"
          :disabled="readonly"
          @change="(value: unknown) => onRow('result_mapping', index, { value: String(value ?? '') })"
        />
        <t-button v-if="!readonly" size="small" variant="text" theme="danger" @click="removeRow('result_mapping', index)">
          <t-icon name="delete" />
        </t-button>
      </div>
      <t-button v-if="!readonly" size="small" variant="outline" @click="addRow('result_mapping')">
        <t-icon name="add" /> 加一条
      </t-button>
      <p class="subprocess-node-config__tip">
        子流程**通过**后按这张表把子表单的值写回父表单（驳回不写）。左边是父表单路径，右边是子表单路径。
      </p>
    </div>
  </t-form-item>
</template>

<script setup lang="ts">
/**
 * 子流程节点属性面板（P3-08）。
 *
 * # 三条纪律（与 TaskNodeConfig / NotifyNodeConfig 同款）
 *
 * 1. **不持有自己的副本**：`cfg` 从 `props.node` 派生 —— 撤销 / 回退时面板自动跟上；
 * 2. **只抛最小 patch**：改 `config.sub` 走 `subConfigPatch`（不覆盖别的 config 分区），
 *    改定义指针抛 `subprocess_def_id`（它是物理列，不在 config 里）；
 * 3. **只读即全灰**：`readonly`（已发布版本）时所有控件禁用，不写半截配置。
 *
 * # 定义下拉的数据源
 *
 * `pageFlowDefs({ status: 'PUBLISHED', page_size: 200 })` —— 只列已发布流程。
 * 为什么不在前端把未发布的灰掉而是**根本不列**：R27 是后端权威，前端少列一条比让用户
 * 存下一条注定发布失败的配置更友好；真要调用草稿流程，后端会 65045 说明原因。
 */
import { computed, onMounted, ref } from 'vue';
import { pageFlowDefs, type FlowDef, type FlowNode } from '@/api/business/flow-api';
import {
  DEFAULT_SUB_CONFIG,
  FAIL_ACTION_OPTIONS,
  VERSION_POLICY_OPTIONS,
  mappingRowsOf,
  rowsToMapping,
  subConfigOf,
  subConfigPatch,
  type FailAction,
  type MappingRow,
  type SubConfig,
  type VersionPolicy,
} from './subprocess-node-config';

const props = withDefaults(
  defineProps<{
    /** 选中节点（`type` 必须是 `SUBPROCESS`，由 `NodeProps` 保证） */
    node?: FlowNode | null;
    /** 只读（已发布版本） */
    readonly?: boolean;
  }>(),
  { node: null, readonly: false },
);

const emit = defineEmits<{
  /** 改节点属性 */
  (event: 'patch', payload: Partial<FlowNode>): void;
}>();

/** 面板状态（从 props 派生） */
const cfg = computed<SubConfig>(() => ({ ...DEFAULT_SUB_CONFIG, ...subConfigOf(props.node) }));

/** 已发布流程列表（下拉数据源） */
const defs = ref<FlowDef[]>([]);
const loading = ref(false);

const defOptions = computed(() =>
  defs.value.map((item) => ({
    label: `${item.flow_name}（${item.flow_code}）`,
    value: item.id,
  })),
);

const versionPolicyHint = computed(
  () => VERSION_POLICY_OPTIONS.find((item) => item.value === (cfg.value.version_policy ?? 'PIN_CURRENT'))?.hint ?? '',
);
const failActionHint = computed(
  () => FAIL_ACTION_OPTIONS.find((item) => item.value === (cfg.value.fail_action ?? 'ABORT'))?.hint ?? '',
);

/** 两套映射的编辑行（空行只在本地存在，保存时才收敛） */
const paramRows = ref<MappingRow[]>([]);
const resultRows = ref<MappingRow[]>([]);

onMounted(async () => {
  paramRows.value = mappingRowsOf(cfg.value.param_mapping);
  resultRows.value = mappingRowsOf(cfg.value.result_mapping);
  loading.value = true;
  try {
    const page = await pageFlowDefs({ status: 'PUBLISHED', page: 1, page_size: 200 });
    defs.value = page.list ?? [];
  } catch {
    // 拉不到列表不该让画布崩：下拉保持空，用户仍可看到「未选择子流程」的提示
    defs.value = [];
  } finally {
    loading.value = false;
  }
});

/** 改 config.sub 的某个键 */
function patch(next: Partial<SubConfig>): void {
  if (props.readonly) {
    return;
  }
  emit('patch', subConfigPatch(props.node, next) as Partial<FlowNode>);
}

/** 选子流程定义（物理列） */
function onDefChange(value: unknown): void {
  if (props.readonly) {
    return;
  }
  const id = String(value ?? '').trim();
  emit('patch', { subprocess_def_id: id === '' ? null : id });
}

/** 映射表：加行（先把本地行写回 config，再追加一条空行） */
function addRow(kind: 'param_mapping' | 'result_mapping'): void {
  const rows = kind === 'param_mapping' ? paramRows : resultRows;
  rows.value = [...rows.value, { key: '', value: '' }];
  syncMapping(kind);
}

/** 映射表：删行 */
function removeRow(kind: 'param_mapping' | 'result_mapping', index: number): void {
  const rows = kind === 'param_mapping' ? paramRows : resultRows;
  rows.value = rows.value.filter((_, position) => position !== index);
  syncMapping(kind);
}

/** 映射表：改某一格 */
function onRow(
  kind: 'param_mapping' | 'result_mapping',
  index: number,
  part: Partial<MappingRow>,
): void {
  const rows = kind === 'param_mapping' ? paramRows : resultRows;
  rows.value = rows.value.map((row, position) => (position === index ? { ...row, ...part } : row));
  syncMapping(kind);
}

/** 本地行 → `config.sub.*_mapping`（空键 / 空值直接丢掉，后端不认空键） */
function syncMapping(kind: 'param_mapping' | 'result_mapping'): void {
  const rows = kind === 'param_mapping' ? paramRows.value : resultRows.value;
  patch({ [kind]: rowsToMapping(rows) } as Partial<SubConfig>);
}
</script>

<style scoped>
.subprocess-node-config__head {
  display: flex;
  align-items: center;
  gap: 8px;
  margin: 4px 0 12px;
  padding-top: 8px;
  border-top: 1px dashed var(--td-component-stroke);
}

.subprocess-node-config__title {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.subprocess-node-config__field {
  width: 100%;
}

.subprocess-node-config__tip {
  margin: 4px 0 0;
  font-size: 12px;
  line-height: 1.5;
  color: var(--td-text-color-placeholder);
}

.subprocess-node-config__mapping {
  width: 100%;
}

.subprocess-node-config__row {
  display: flex;
  align-items: center;
  gap: 6px;
  margin-bottom: 6px;
}

.subprocess-node-config__arrow {
  color: var(--td-text-color-placeholder);
}
</style>
