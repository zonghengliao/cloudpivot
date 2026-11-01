<template>
  <div class="flow-editor">
    <!-- ==================== 顶部工具条 ==================== -->
    <div class="flow-editor__bar">
      <div class="flow-editor__bar-left">
        <t-button variant="text" size="small" @click="goBack">
          <template #icon><t-icon name="chevron-left" /></template>
          返回列表
        </t-button>
        <div class="flow-editor__title">
          <span class="flow-editor__name">{{ defDetail?.flow_name || '流程编辑器' }}</span>
          <span class="flow-editor__code">{{ defDetail?.flow_code }}</span>
          <StatusBadge v-if="defDetail" domain="flow-def" :status="defDetail.status" />
        </div>
        <t-select
          v-if="versions.length > 0"
          :value="currentVid ?? undefined"
          :options="versionOptions"
          size="small"
          class="flow-editor__version-select"
          @change="onVersionChange"
        />
        <t-tag v-if="dirty" size="small" theme="warning" variant="light">未保存</t-tag>
      </div>

      <div class="flow-editor__bar-right">
        <t-space size="small">
          <t-button size="small" variant="outline" :disabled="readonly || !history.canUndo" title="Ctrl+Z" @click="undo">
            <template #icon><t-icon name="rollback" /></template>
            撤销
          </t-button>
          <t-button size="small" variant="outline" :disabled="readonly || !history.canRedo" title="Ctrl+Shift+Z" @click="redo">
            <template #icon><t-icon name="refresh" /></template>
            重做
          </t-button>
          <t-button size="small" variant="outline" :disabled="readonly" title="适应画布" @click="canvasRef?.fitView()">适配</t-button>
          <t-button
            v-privilege="[PERM.flowUpdate]"
            size="small"
            theme="primary"
            :loading="saving"
            :disabled="readonly"
            @click="onSave"
          >
            保存
          </t-button>
          <t-button
            v-privilege="[PERM.flowUpdate]"
            size="small"
            variant="outline"
            :loading="validating"
            :disabled="readonly"
            @click="onValidate"
          >
            校验
          </t-button>
          <t-tooltip :content="publishBlockReason || '发布前会自动保存并跑完整校验'">
            <t-button
              v-privilege="[PERM.flowPublish]"
              size="small"
              theme="warning"
              :loading="publishing"
              :disabled="readonly || issueCount > 0"
              @click="onPublish"
            >
              发布
            </t-button>
          </t-tooltip>
        </t-space>
      </div>
    </div>

    <!-- ==================== 只读横幅（已发布版本） ==================== -->
    <t-alert v-if="readonly" class="flow-editor__alert" theme="warning" :message="readonlyMessage">
      <template #operation>
        <t-button
          v-privilege="[PERM.flowUpdate]"
          size="small"
          theme="primary"
          :loading="drafting"
          @click="onCreateDraft"
        >
          另存为新草稿
        </t-button>
      </template>
    </t-alert>

    <!-- 保存回执里的警告（不阻断保存，但发布会拦） -->
    <t-alert
      v-if="saveWarnings.length > 0"
      class="flow-editor__alert"
      theme="warning"
      :message="`已保存，但有 ${saveWarnings.length} 条发布会拦的问题`"
    >
      <template #operation>
        <t-link theme="primary" @click="warningVisible = true">查看</t-link>
      </template>
    </t-alert>

    <!-- ==================== 三栏主体 ==================== -->
    <div class="flow-editor__body">
      <!-- 左：节点面板 -->
      <aside class="flow-editor__palette">
        <div class="flow-editor__palette-title">节点面板</div>
        <div
          v-for="item in nodeTypes"
          :key="item.type"
          class="palette-item"
          :class="[`palette-item--${item.type.toLowerCase()}`, { 'is-disabled': readonly }]"
          :data-node-type="item.type"
          @pointerdown="onPaletteDown($event, item.type)"
        >
          <span class="palette-item__shape" />
          <span class="palette-item__text">
            <span class="palette-item__label">{{ item.label }}</span>
            <span class="palette-item__hint">{{ item.hint }}</span>
          </span>
        </div>
        <div class="flow-editor__palette-tip">
          拖到画布即可添加；已发布版本为只读。
        </div>
      </aside>

      <!-- 中：画布 -->
      <main class="flow-editor__canvas">
        <FlowCanvas
          ref="canvasRef"
          :nodes="graph.nodes"
          :edges="graph.edges"
          :readonly="readonly"
          :selected-key="selectedKey"
          :selected-edge-key="selectedEdgeKey"
          :issue-node-keys="issueNodeKeys"
          :can-undo-layout="layoutSnapshot !== null"
          :show-minimap="issues.length === 0"
          @select-node="onSelectNode"
          @select-edge="onSelectEdge"
          @move-node="onMoveNode"
          @connect="onConnect"
          @delete-selected="onDeleteSelected"
          @undo="undo"
          @redo="redo"
          @nudge="onNudge"
          @auto-layout="onAutoLayout"
          @undo-layout="onUndoLayout"
        />

        <!-- 校验问题列表（每条可点定位） -->
        <div v-if="issues.length > 0" class="issue-board">
          <div class="issue-board__head">
            <span>
              <t-icon name="error-circle" class="issue-board__icon" />
              校验未通过 {{ issues.length }} 条
            </span>
            <t-link theme="default" size="small" @click="issues = []">收起</t-link>
          </div>
          <ul class="issue-board__list">
            <li v-for="(issue, index) in issues" :key="`${issue.rule}-${index}`" class="issue-board__item" @click="locateIssue(issue)">
              <t-tag size="small" theme="danger" variant="light">{{ issue.rule }}</t-tag>
              <span class="issue-board__text">{{ issue.message }}</span>
              <span v-if="issue.nodes.length > 0" class="issue-board__locate">定位</span>
            </li>
          </ul>
        </div>
      </main>

      <!-- 右：属性面板 -->
      <aside class="flow-editor__props">
        <NodeProps
          :node="selectedNode"
          :edge="selectedEdge"
          :target-node="selectedEdgeTarget"
          :readonly="readonly"
          :biz-type="defDetail?.biz_type ?? null"
          :form-fields="formFields"
          @patch-node="onPatchNode"
          @patch-edge="onPatchEdge"
        />
      </aside>
    </div>

    <!-- 拖入节点的跟随幽灵 -->
    <div
      v-if="paletteDrag"
      class="flow-editor__ghost"
      :style="{ left: `${paletteDrag.x}px`, top: `${paletteDrag.y}px` }"
    >
      {{ paletteDrag.label }}
    </div>

    <!-- 保存警告详情 -->
    <t-dialog v-model:visible="warningVisible" header="发布前需要处理的问题" width="560px" :footer="false">
      <ul class="warning-list">
        <li v-for="(item, index) in saveWarnings" :key="index">{{ item }}</li>
      </ul>
    </t-dialog>

    <!-- 离开拦截 -->
    <t-dialog
      v-model:visible="leaveVisible"
      header="有未保存的修改"
      theme="warning"
      width="440px"
      :confirm-btn="{ content: '离开（丢弃修改）', theme: 'warning' }"
      cancel-btn="留在这里"
      @confirm="confirmLeave"
    >
      <p>当前图有未保存的修改，离开后这些修改会丢失。</p>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 流程编辑器（卡面页面 2 / `/flow/def/{id}/edit`）—— 本卡最重的部分。
 *
 * # 结构
 *
 * 左：节点面板（拖入）｜中：**纯 SVG 自绘画布**（`FlowCanvas.vue`）｜右：属性面板（`NodeProps.vue`）。
 * 画布不引任何重型图库（红线：不用 bpmn-js / jsPlumb / vue-flow）。
 *
 * # 三个必须做对的地方
 *
 * 1. **撤销栈是命令栈不是快照栈**（架构 §11.5）：一次拖拽 = 一条 `move` 命令，
 *    一次属性输入 = 一条 `patch-node`；上限 100 步（卡面要求 ≥ 50）。
 * 2. **校验以服务端为准**（§11.6）：点「校验」= 先保存再 `POST /version/{vid}/validate`，
 *    拿回 `issues[].nodes` 在画布上高亮，列表每条可点定位。前端不自己判环。
 * 3. **已发布版本只读**（红线）：`version.editable === false` 时画布 `readonly`，
 *    界面提示「已发布，如需修改请另存草稿」，另存走 `POST /def/{id}/draft`。
 *
 * # 保存 / 校验 / 发布的顺序
 *
 * 「校验」和「发布」都会**先保存**：`validate` 校验的是库里已保存的图，
 * 不先保存就会出现「画布上是新的、校验的是旧的」。
 */
import { computed, onMounted, ref } from 'vue';
import { onBeforeRouteLeave, useRoute, useRouter } from 'vue-router';
import { MessagePlugin, DialogPlugin } from 'tdesign-vue-next';
import {
  createFlowDraft,
  getFlowDef,
  getFlowVersion,
  listFlowVersions,
  pageFlowInstances,
  publishFlowDef,
  saveFlowGraph,
  validateFlowVersion,
  type FlowAssigneeRowKind,
  type FlowDef,
  type FlowEdge,
  type FlowEdgeReq,
  type FlowIssue,
  type FlowNode,
  type FlowNodeReq,
  type FlowNodeType,
  type FlowVersion,
  type FlowVersionSummary,
} from '@/api/business/flow-api';
import { ApiError } from '@/api/request';
import { PERM } from '@/constants/permissions';
import StatusBadge from '@/views/business/task/components/StatusBadge.vue';
import FlowCanvas from '@/views/business/flow/components/FlowCanvas.vue';
import NodeProps from '@/views/business/flow/components/NodeProps.vue';
import { FlowHistory, NODE_TYPES, autoLayout, checkEdgeAllowed, makeEdge, makeNode, nodeSize, snapGrid, type FlowCommand, type MutableGraph, applyCommand, revertCommand } from '@/views/business/flow/graph';
import { computeAutoLayout } from '@/views/business/flow/auto-layout';

/** 路由 */
const route = useRoute();
const router = useRouter();

/** 流程定义 id（路由参数） */
const defId = computed(() => String(route.params.id ?? ''));

/** 画布组件引用 */
const canvasRef = ref<InstanceType<typeof FlowCanvas> | null>(null);

/** 流程定义详情 */
const defDetail = ref<FlowDef | null>(null);
/** 版本列表 */
const versions = ref<FlowVersionSummary[]>([]);
/** 当前版本详情 */
const version = ref<FlowVersion | null>(null);
/** 当前版本 id */
const currentVid = ref<string | null>(null);

/** 图（节点 + 连线；命令栈直接改它） */
const graph = reactive<MutableGraph>({ nodes: [], edges: [] });

/** 撤销栈 */
const history = reactive(new FlowHistory());

/** 选中态 */
const selectedKey = ref<string | null>(null);
const selectedEdgeKey = ref<string | null>(null);

/**
 * 最近一次自动布局前的坐标快照（null = 没布局过 / 已撤销）。
 * 非 null 时画布右上角出现「撤销布局」按钮。
 */
const layoutSnapshot = ref<Array<{ key: string; x: number; y: number }> | null>(null);

/** 校验结果 */
const issues = ref<FlowIssue[]>([]);
/** 保存回执里的警告 */
const saveWarnings = ref<string[]>([]);
/** 警告弹窗 */
const warningVisible = ref(false);

/** 加载 / 保存 / 校验 / 发布 / 另存草稿 的忙碌态 */
const loading = ref(false);
const saving = ref(false);
const validating = ref(false);
const publishing = ref(false);
const drafting = ref(false);

/** 未保存标记 */
const dirty = ref(false);

/** 离开拦截 */
const leaveVisible = ref(false);
/** 用户已确认丢弃修改（放行一次路由跳转，否则确认后还会被守卫再拦一次） */
const allowLeave = ref(false);
/** 待跳转的目标（用户确认丢弃后跳过去） */
let pendingLeave: (() => void) | null = null;

/** 节点类型（左侧面板） */
const nodeTypes = NODE_TYPES;

/** 只读：版本不是 DRAFT（卡面红线：发布后编辑器只读） */
const readonly = computed(() => version.value !== null && !version.value.editable);

/** 只读提示文案 */
const readonlyMessage = computed(() =>
  version.value?.version_status === 'PUBLISHED'
    ? `v${version.value.version_no} 已发布，如需修改请另存草稿（已发布版本的图不可静默修改）。`
    : '该版本不可编辑，如需修改请另存草稿。',
);

/** 校验问题条数 */
const issueCount = computed(() => issues.value.length);

/** 发布按钮禁用原因 */
const publishBlockReason = computed(() => {
  if (readonly.value) {
    return '已发布版本不能再次发布，请先另存草稿';
  }
  if (issueCount.value > 0) {
    return `还有 ${issueCount.value} 条校验未通过，逐条处理后再发布`;
  }
  return '';
});

/** 需要高亮的节点 key（后端 issues[].nodes 直出） */
const issueNodeKeys = computed(() => {
  const keys = new Set<string>();
  for (const issue of issues.value) {
    for (const key of issue.nodes ?? []) {
      keys.add(key);
    }
  }
  return Array.from(keys);
});

/** 选中的节点对象 */
const selectedNode = computed(() => graph.nodes.find((node) => node.key === selectedKey.value) ?? null);
/** 选中的连线对象 */
const selectedEdge = computed(() => graph.edges.find((edge) => edge.key === selectedEdgeKey.value) ?? null);

/**
 * 选中连线的**目标节点**（P1-B3：规则预览的 THEN 段 / then 目标校验要用）。
 *
 * 连线的语义是「WHEN 条件 → 走到这个节点」，审批人配在节点上，
 * 所以「由谁审批」必须从 `edge.to` 反查节点，不能从连线自己身上找。
 */
const selectedEdgeTarget = computed(() => {
  const edge = selectedEdge.value;
  if (!edge) return null;
  return graph.nodes.find((node) => node.key === edge.to) ?? null;
});

/** 版本下拉选项 */
const versionOptions = computed(() =>
  versions.value.map((item) => ({
    label: `v${item.version_no}（${versionStatusLabel(item.version_status)}）${item.is_current ? ' · 当前' : ''}`,
    value: item.id,
  })),
);

/** 版本状态下拉文案 */
function versionStatusLabel(status: string): string {
  if (status === 'DRAFT') {
    return '草稿';
  }
  if (status === 'PUBLISHED') {
    return '已发布';
  }
  if (status === 'ARCHIVED') {
    return '已归档';
  }
  return status;
}

// ==================== 加载 ====================

/**
 * `form.*` 变量候选（P-FLOW-1C）。
 *
 * # 为什么从「历史实例的 form_data」推，而不是查一个 `form-fields` 接口
 *
 * 后端没有 `form-fields`（1A 全仓 grep 零命中），`biz_flow_def` 上也没存表单 schema。
 * 按卡面「宁可少列，不可伪造候选」：这里读**最近 20 个本流程实例**的 `form_data` 顶层键，
 * 一个实例都没有时就是空数组 —— 面板上只剩「手输」，绝不凭空造字段名。
 */
const formFields = ref<string[]>([]);

/** 拉历史实例的 `form_data` 键（失败静默：只是少几个候选，不影响编辑） */
async function loadFormFields(): Promise<void> {
  const code = defDetail.value?.flow_code;
  if (!code) {
    return;
  }
  try {
    const page = await pageFlowInstances({ flow_code: code, page_size: 20 });
    const keys = new Set<string>();
    for (const item of page.list ?? []) {
      const data = (item.form_data ?? {}) as Record<string, unknown>;
      for (const key of Object.keys(data)) {
        keys.add(key);
      }
    }
    formFields.value = Array.from(keys).sort();
  } catch {
    formFields.value = [];
  }
}

/** 载入流程定义 + 版本列表，并选中合适的版本 */
async function bootstrap(): Promise<void> {
  loading.value = true;
  try {
    defDetail.value = await getFlowDef(defId.value);
    void loadFormFields();
    const list = await listFlowVersions(defId.value);
    versions.value = list.list ?? [];
    const queryVid = typeof route.query.vid === 'string' ? route.query.vid : '';
    const target =
      (queryVid && versions.value.find((item) => item.id === queryVid)?.id) ||
      // 优先打开最新草稿（可编辑），没有草稿就打开当前发布版（只读）
      [...versions.value]
        .filter((item) => item.version_status === 'DRAFT')
        .sort((a, b) => b.version_no - a.version_no)[0]?.id ||
      versions.value.find((item) => item.is_current)?.id ||
      versions.value[0]?.id ||
      null;
    if (!target) {
      MessagePlugin.warning('该流程还没有版本，请重新创建');
      return;
    }
    await loadVersion(target);
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '流程加载失败');
  } finally {
    loading.value = false;
  }
}

/**
 * 载入某个版本的整图。
 *
 * 会把后端返回的节点 / 连线**深拷贝**进 `graph`：命令栈与拖拽直接改本地对象，
 * 不能改到 `version.value.nodes`（那是「服务端最后一次的样子」，保存回滚要靠它对照）。
 */
async function loadVersion(vid: string): Promise<void> {
  const data = await getFlowVersion(vid);
  version.value = data;
  currentVid.value = data.id;
  graph.nodes = (data.nodes ?? []).map((node) => ({ ...node, assignees: [...(node.assignees ?? [])] }));
  graph.edges = (data.edges ?? []).map((edge) => ({ ...edge }));
  history.clear();
  layoutSnapshot.value = null;
  issues.value = [];
  saveWarnings.value = [];
  selectedKey.value = null;
  selectedEdgeKey.value = null;
  dirty.value = false;
  // 新建流程时后端给的默认图（开始 / 结束）坐标是 (0,0) —— 两个节点会叠在一起，
  // 用户打开编辑器看到的是「一个点」。这里做一次**纯展示层**的分层排版：
  // 不写入后端、不进撤销栈、不标记 dirty（用户保存时才落库）。
  if (graph.nodes.length > 1 && graph.nodes.every((node) => node.x === 0 && node.y === 0)) {
    const positions = autoLayout(graph.nodes, graph.edges);
    for (const node of graph.nodes) {
      const target = positions.get(node.key);
      if (target) {
        node.x = target.x;
        node.y = target.y;
      }
    }
  }
  requestAnimationFrame(() => canvasRef.value?.fitView());
}

/** 切换版本（有未保存修改先问一句） */
function onVersionChange(value: unknown): void {
  const vid = String(value ?? '');
  if (!vid || vid === currentVid.value) {
    return;
  }
  const run = (): void => {
    void loadVersion(vid).catch((error: unknown) => {
      MessagePlugin.error(error instanceof ApiError ? error.message : '版本加载失败');
    });
  };
  if (dirty.value) {
    pendingLeave = run;
    leaveVisible.value = true;
    return;
  }
  run();
}

// ==================== 命令提交（统一入口） ====================

/** 记一条命令并应用（所有图变更都必须走这里，撤销栈才不会漏） */
function commit(command: FlowCommand): void {
  history.push(command);
  applyCommand(graph, command);
  dirty.value = true;
}

/** 撤销 */
function undo(): void {
  if (readonly.value) {
    return;
  }
  const command = history.popUndo();
  if (!command) {
    return;
  }
  revertCommand(graph, command);
  dirty.value = true;
}

/** 重做 */
function redo(): void {
  if (readonly.value) {
    return;
  }
  const command = history.popRedo();
  if (!command) {
    return;
  }
  applyCommand(graph, command);
  dirty.value = true;
}

// ==================== 画布事件 ====================

/** 选中节点（清掉连线选中） */
function onSelectNode(key: string | null): void {
  selectedKey.value = key;
  selectedEdgeKey.value = null;
}

/** 选中连线 */
function onSelectEdge(key: string | null): void {
  selectedEdgeKey.value = key;
  selectedKey.value = null;
}

/** 拖拽结束：提交一次 move */
function onMoveNode(payload: { key: string; x: number; y: number }): void {
  const node = graph.nodes.find((item) => item.key === payload.key);
  if (!node) {
    return;
  }
  const from = { x: node.x, y: node.y };
  const to = { x: snapGrid(payload.x), y: snapGrid(payload.y) };
  if (from.x === to.x && from.y === to.y) {
    return;
  }
  commit({ kind: 'move', key: payload.key, from, to });
}

/** 方向键微调 */
function onNudge(payload: { key: string; dx: number; dy: number }): void {
  const node = graph.nodes.find((item) => item.key === payload.key);
  if (!node) {
    return;
  }
  commit({
    kind: 'move',
    key: payload.key,
    from: { x: node.x, y: node.y },
    to: { x: Math.max(0, node.x + payload.dx), y: Math.max(0, node.y + payload.dy) },
  });
}

/** 连线：自环 / 重复边在提交前拦截（卡面验收 6） */
function onConnect(payload: { from: string; to: string }): void {
  if (readonly.value) {
    return;
  }
  const rejected = checkEdgeAllowed(graph.edges, payload.from, payload.to);
  if (rejected) {
    MessagePlugin.warning(rejected);
    return;
  }
  commit({ kind: 'add-edge', edge: makeEdge(payload.from, payload.to, graph.edges.map((edge) => edge.key)) });
  MessagePlugin.success('已连线');
}

/** 删除选中（节点连带删边；边直接删） */
function onDeleteSelected(): void {
  if (readonly.value) {
    return;
  }
  if (selectedKey.value) {
    const index = graph.nodes.findIndex((node) => node.key === selectedKey.value);
    if (index < 0) {
      return;
    }
    const node = graph.nodes[index];
    const related = graph.edges
      .map((edge, order) => ({ edge, index: order }))
      .filter((item) => item.edge.from === node.key || item.edge.to === node.key);
    commit({ kind: 'del-node', node: { ...node }, index, edges: related.map((item) => ({ edge: { ...item.edge }, index: item.index })) });
    selectedKey.value = null;
    MessagePlugin.success(related.length > 0 ? `已删除节点及 ${related.length} 条连线` : '已删除节点');
    return;
  }
  if (selectedEdgeKey.value) {
    const index = graph.edges.findIndex((edge) => edge.key === selectedEdgeKey.value);
    if (index < 0) {
      return;
    }
    commit({ kind: 'del-edge', edge: { ...graph.edges[index] }, index });
    selectedEdgeKey.value = null;
    MessagePlugin.success('已删除连线');
  }
}

/**
 * 自动布局（P12：dagre 树状布局 + 二次确认 + 可撤销）。
 *
 * 流程：确认框 → `computeAutoLayout`（纯算法）→ 包成一条 `layout` 命令压栈
 * → `fitView()` 让全图可见 → 记下布局前的坐标供「撤销布局」用。
 *
 * 方向取 `LR`：本画布的锚点是**固定左进右出**，TB（上→下）会让每条边从右侧出来
 * 再绕回下一个节点的左侧，观感很差。
 */
function onAutoLayout(): void {
  if (readonly.value || graph.nodes.length === 0) {
    return;
  }
  const dialog = DialogPlugin.confirm({
    header: '自动布局',
    body: '这会按层级重排所有节点的位置（布局后可用「撤销布局」还原），是否继续？',
    confirmBtn: '开始重排',
    cancelBtn: '取消',
    onConfirm: () => {
      dialog.destroy();
      doAutoLayout();
    },
    onCancel: () => dialog.destroy(),
    onClose: () => dialog.destroy(),
  });
}

/** 真正执行布局（只在用户确认后调用；不自动触发） */
function doAutoLayout(): void {
  if (readonly.value || graph.nodes.length === 0) {
    return;
  }
  const positions = computeAutoLayout(
    graph.nodes.map((node) => ({ id: node.key, width: nodeSize(node.type).w, height: nodeSize(node.type).h })),
    graph.edges.map((edge) => ({ source: edge.from, target: edge.to })),
    'LR',
  );
  const moves: Array<{ key: string; from: { x: number; y: number }; to: { x: number; y: number } }> = [];
  const snapshot: Array<{ key: string; x: number; y: number }> = [];
  for (const node of graph.nodes) {
    // 快照覆盖**所有**节点（包括坐标没变的），撤销时才能整图还原
    snapshot.push({ key: node.key, x: node.x, y: node.y });
    const target = positions.get(node.key);
    if (target && (target.x !== node.x || target.y !== node.y)) {
      moves.push({ key: node.key, from: { x: node.x, y: node.y }, to: target });
    }
  }
  if (moves.length === 0) {
    layoutSnapshot.value = null;
    MessagePlugin.info('节点已经很整齐了，无需重排');
    return;
  }
  commit({ kind: 'layout', moves });
  layoutSnapshot.value = snapshot;
  requestAnimationFrame(() => canvasRef.value?.fitView());
  MessagePlugin.success(`已重排 ${moves.length} 个节点`);
}

/**
 * 撤销布局：把坐标还原成「布局前」的那一份。
 *
 * 不复用 Ctrl+Z 的原因是「撤销」是**最近一条命令**语义，而用户点完布局很可能又拖了一下节点，
 * 那时 Ctrl+Z 只回退拖拽。这里按快照整图回写，并同样压一条 `layout` 命令（这次撤销本身也能再撤销）。
 */
function onUndoLayout(): void {
  const snapshot = layoutSnapshot.value;
  if (!snapshot || readonly.value) {
    return;
  }
  const moves: Array<{ key: string; from: { x: number; y: number }; to: { x: number; y: number } }> = [];
  for (const item of snapshot) {
    const node = graph.nodes.find((candidate) => candidate.key === item.key);
    if (node && (node.x !== item.x || node.y !== item.y)) {
      moves.push({ key: item.key, from: { x: node.x, y: node.y }, to: { x: item.x, y: item.y } });
    }
  }
  layoutSnapshot.value = null;
  if (moves.length === 0) {
    MessagePlugin.info('节点位置没有变化');
    return;
  }
  commit({ kind: 'layout', moves });
  requestAnimationFrame(() => canvasRef.value?.fitView());
  MessagePlugin.success('已撤销布局');
}

// ==================== 属性面板事件 ====================

/** 改节点属性：只记被改的字段（撤销时精确回退） */
function onPatchNode(payload: { key: string; patch: Partial<FlowNode> }): void {
  const node = graph.nodes.find((item) => item.key === payload.key);
  if (!node) {
    return;
  }
  const before: Record<string, unknown> = {};
  const record = node as unknown as Record<string, unknown>;
  for (const field of Object.keys(payload.patch)) {
    before[field] = record[field];
  }
  commit({
    kind: 'patch-node',
    key: payload.key,
    name: payload.patch.name ?? node.name,
    before: before as Partial<FlowNode>,
    after: payload.patch,
  });
}

/** 改连线属性 */
function onPatchEdge(payload: { key: string; patch: Partial<FlowEdge> }): void {
  const edge = graph.edges.find((item) => item.key === payload.key);
  if (!edge) {
    return;
  }
  const before: Record<string, unknown> = {};
  const record = edge as unknown as Record<string, unknown>;
  for (const field of Object.keys(payload.patch)) {
    before[field] = record[field];
  }
  commit({
    kind: 'patch-edge',
    key: payload.key,
    before: before as Partial<FlowEdge>,
    after: payload.patch,
  });
}

// ==================== 左侧面板拖入节点 ====================

/** 拖拽中的幽灵 */
const paletteDrag = ref<{ type: FlowNodeType; label: string; x: number; y: number } | null>(null);

/** 面板按下：开始拖（pointer events，能拿到精确落点） */
function onPaletteDown(event: PointerEvent, type: FlowNodeType): void {
  if (readonly.value) {
    MessagePlugin.warning('已发布版本只读，请先另存草稿');
    return;
  }
  if (event.button !== 0) {
    return;
  }
  // 阻止浏览器把这次按下当成「滚动面板」的手势起点（否则会收到 pointercancel）
  event.preventDefault();
  const meta = nodeTypes.find((item) => item.type === type);
  paletteDrag.value = { type, label: meta?.label ?? type, x: event.clientX, y: event.clientY };
  window.addEventListener('pointermove', onPaletteMove);
  window.addEventListener('pointerup', onPaletteUp);
  window.addEventListener('pointercancel', onPaletteCancel);
}

/** 取消拖动（浏览器收走手势时）：清理状态，不创建节点 */
function onPaletteCancel(): void {
  window.removeEventListener('pointermove', onPaletteMove);
  window.removeEventListener('pointerup', onPaletteUp);
  window.removeEventListener('pointercancel', onPaletteCancel);
  paletteDrag.value = null;
}

/** 面板拖拽跟随 */
function onPaletteMove(event: PointerEvent): void {
  if (!paletteDrag.value) {
    return;
  }
  paletteDrag.value = { ...paletteDrag.value, x: event.clientX, y: event.clientY };
}

/** 松手：落在画布内就生成节点（坐标用画布自己的逆变换算，不用手估） */
function onPaletteUp(event: PointerEvent): void {
  window.removeEventListener('pointermove', onPaletteMove);
  window.removeEventListener('pointerup', onPaletteUp);
  window.removeEventListener('pointercancel', onPaletteCancel);
  const drag = paletteDrag.value;
  paletteDrag.value = null;
  if (!drag || readonly.value) {
    return;
  }
  const host = document.querySelector('.flow-editor__canvas');
  const rect = host?.getBoundingClientRect();
  if (!rect || event.clientX < rect.left || event.clientX > rect.right || event.clientY < rect.top || event.clientY > rect.bottom) {
    return;
  }
  const point = canvasRef.value?.screenToCanvas(event.clientX, event.clientY);
  if (!point) {
    return;
  }
  if (graph.nodes.length >= 200) {
    MessagePlugin.warning('画布最多 200 个节点（后端发布上限）');
    return;
  }
  const node = makeNode(drag.type, point, graph.nodes.map((item) => item.key));
  commit({ kind: 'add-node', node });
  selectedKey.value = node.key;
  selectedEdgeKey.value = null;
}

// ==================== 保存 / 校验 / 发布 ====================

/**
 * 组装保存载荷（节点 / 连线字段名与后端 `GraphNodeReq` / `GraphEdgeReq` 逐字对齐）。
 *
 * 两处必须显式转换，不能直接展开：
 * - `type`：后端字段名就是 `type`（serde rename），TS 侧是联合字面量；
 * - `assignees[].type`：后端只接受 `USER|ROLE|DEPT_LEADER` 三种可落行来源，
 *   而响应里是宽类型 `string`（见 `FlowAssignee`），这里按行过滤一遍。
 *
 * `sort_order` 按数组顺序补齐，便于后端稳定排序。
 */
function buildPayload(): { nodes: FlowNodeReq[]; edges: FlowEdgeReq[] } {
  return {
    nodes: graph.nodes.map((node, index) => ({
      key: node.key,
      name: node.name,
      type: node.type as FlowNodeType,
      x: node.x,
      y: node.y,
      sort_order: index,
      assignee_type: node.assignee_type,
      superior_levels: node.superior_levels,
      form_var_key: node.form_var_key,
      approval_mode: node.approval_mode,
      approve_ratio: node.approve_ratio,
      reject_veto: node.reject_veto,
      empty_assignee: node.empty_assignee,
      self_approval: node.self_approval,
      allow_robot: node.allow_robot,
      join_mode: node.join_mode,
      fork_mode: node.fork_mode,
      timeout_hours: node.timeout_hours,
      timeout_action: node.timeout_action,
      remind_before_hours: node.remind_before_hours,
      max_remind_times: node.max_remind_times,
      cc_on_approved: node.cc_on_approved,
      config: node.config,
      assignees: (node.assignees ?? [])
        .filter((assignee) => ['USER', 'ROLE', 'DEPT_LEADER'].includes(assignee.type))
        .map((assignee, order) => ({
          type: assignee.type as FlowAssigneeRowKind,
          ref_id: assignee.ref_id,
          sort_order: assignee.sort_order ?? order,
        })),
    })),
    edges: graph.edges.map((edge) => ({
      key: edge.key,
      from: edge.from,
      to: edge.to,
      condition: edge.condition ?? null,
      label: edge.label,
      priority: edge.priority,
      is_default: edge.is_default,
    })),
  };
}

/** 保存整图；返回是否成功 */
async function saveGraph(silent = false): Promise<boolean> {
  if (!currentVid.value || readonly.value) {
    return false;
  }
  saving.value = true;
  try {
    const payload = buildPayload();
    const result = await saveFlowGraph(currentVid.value, { nodes: payload.nodes, edges: payload.edges });
    dirty.value = false;
    saveWarnings.value = result.warnings ?? [];
    if (!silent) {
      MessagePlugin.success(
        `已保存 v${result.version_no}（${result.node_count} 节点 / ${result.edge_count} 连线）` +
          (saveWarnings.value.length > 0 ? `，${saveWarnings.value.length} 条待处理` : ''),
      );
    }
    // 版本列表里的节点数 / 边数会变，刷新一下下拉
    void refreshVersions();
    return true;
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '保存失败');
    return false;
  } finally {
    saving.value = false;
  }
}

/** 只刷新版本摘要（不动当前图） */
async function refreshVersions(): Promise<void> {
  try {
    const list = await listFlowVersions(defId.value);
    versions.value = list.list ?? [];
  } catch {
    // 版本列表刷新失败不影响编辑
  }
}

/** 点「保存」 */
async function onSave(): Promise<void> {
  await saveGraph();
}

/** 点「校验」：先保存再校验（validate 校验的是库里的图） */
async function onValidate(): Promise<void> {
  if (!currentVid.value) {
    return;
  }
  validating.value = true;
  try {
    if (dirty.value) {
      const saved = await saveGraph(true);
      if (!saved) {
        return;
      }
    }
    const result = await validateFlowVersion(currentVid.value);
    issues.value = result.issues ?? [];
    if (result.passed) {
      MessagePlugin.success(`校验通过（${result.node_count} 节点 / ${result.edge_count} 连线）`);
    } else {
      MessagePlugin.warning(`校验未通过 ${issues.value.length} 条，已在画布上标出问题节点`);
      const first = issues.value[0];
      if (first && first.nodes.length > 0) {
        canvasRef.value?.centerOn(first.nodes[0]);
      }
    }
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '校验失败');
  } finally {
    validating.value = false;
  }
}

/** 点「发布」：保存 → 校验 → 全过才发布 */
async function onPublish(): Promise<void> {
  if (!currentVid.value || readonly.value) {
    return;
  }
  publishing.value = true;
  try {
    if (dirty.value) {
      const saved = await saveGraph(true);
      if (!saved) {
        return;
      }
    }
    const checked = await validateFlowVersion(currentVid.value);
    issues.value = checked.issues ?? [];
    if (!checked.passed) {
      MessagePlugin.warning(`还有 ${issues.value.length} 条校验未通过，不能发布`);
      return;
    }
    const result = await publishFlowDef(defId.value);
    MessagePlugin.success(`已发布 v${result.version_no}（图 hash ${result.graph_hash.slice(0, 12)}…）`);
    // 发布后该版本变 PUBLISHED → 编辑器自动切只读（重新加载即可拿到 editable=false）
    await bootstrap();
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '发布失败');
  } finally {
    publishing.value = false;
  }
}

/** 另存为新草稿（编辑已发布版本的唯一正确姿势） */
async function onCreateDraft(): Promise<void> {
  drafting.value = true;
  try {
    const result = await createFlowDraft(defId.value, { source_version_id: currentVid.value });
    MessagePlugin.success(`已复制为 v${result.version_no} 草稿（${result.node_count} 节点）`);
    await bootstrap();
    const target = versions.value.find((item) => item.id === result.version_id);
    if (target) {
      await loadVersion(target.id);
    }
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '另存草稿失败');
  } finally {
    drafting.value = false;
  }
}

// ==================== 校验定位 ====================

/** 点问题条目：把对应节点居中 */
function locateIssue(issue: FlowIssue): void {
  const key = issue.nodes?.[0];
  if (!key) {
    return;
  }
  selectedKey.value = key;
  selectedEdgeKey.value = null;
  canvasRef.value?.centerOn(key);
}

// ==================== 离开拦截 ====================

/** 返回列表（有未保存修改先问） */
function goBack(): void {
  const run = (): void => {
    void router.push('/flow/def');
  };
  if (dirty.value) {
    pendingLeave = run;
    leaveVisible.value = true;
    return;
  }
  run();
}

/** 确认丢弃修改 */
function confirmLeave(): void {
  leaveVisible.value = false;
  const run = pendingLeave;
  pendingLeave = null;
  // 放行一次：dirty 仍是 true，不放行的话路由守卫会把这次确认后的跳转再拦一遍
  allowLeave.value = true;
  if (run) {
    run();
  } else {
    void router.push('/flow/def');
  }
  // 下一帧恢复，避免后续真正需要拦截时被放行
  requestAnimationFrame(() => {
    allowLeave.value = false;
  });
}

onBeforeRouteLeave(() => {
  if (allowLeave.value || !dirty.value) {
    return true;
  }
  pendingLeave = null;
  leaveVisible.value = true;
  return false;
});

onMounted(() => {
  void bootstrap();
});
</script>

<style scoped lang="less">
.flow-editor {
  display: flex;
  flex-direction: column;
  gap: 10px;
  height: calc(100vh - 132px);

  &__bar {
    display: flex;
    gap: 12px;
    align-items: center;
    justify-content: space-between;
    padding: 8px 12px;
    background: var(--td-bg-color-container);
    border: 1px solid var(--td-component-stroke);
    border-radius: 8px;
  }

  &__bar-left,
  &__bar-right {
    display: flex;
    gap: 10px;
    align-items: center;
    min-width: 0;
  }

  &__title {
    display: flex;
    gap: 8px;
    align-items: center;
    min-width: 0;
  }

  &__name {
    overflow: hidden;
    font-size: 15px;
    font-weight: 600;
    color: var(--td-text-color-primary);
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  &__code {
    font-family: var(--app-font-mono, monospace);
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__version-select {
    width: 190px;
  }

  &__alert {
    margin: 0;
    border-radius: 6px;
  }

  &__body {
    display: flex;
    flex: 1;
    gap: 10px;
    min-height: 0;
  }

  &__palette {
    display: flex;
    flex-direction: column;
    gap: 6px;
    width: 168px;
    flex: none;
    padding: 10px;
    background: var(--td-bg-color-container);
    border: 1px solid var(--td-component-stroke);
    border-radius: 8px;
    overflow-y: auto;
  }

  &__palette-title {
    margin-bottom: 2px;
    font-size: 12px;
    font-weight: 600;
    color: var(--td-text-color-secondary);
  }

  &__palette-tip {
    padding-top: 8px;
    margin-top: auto;
    font-size: 11px;
    line-height: 1.5;
    color: var(--td-text-color-placeholder);
    border-top: 1px solid var(--td-component-stroke);
  }

  &__canvas {
    position: relative;
    flex: 1;
    min-width: 0;
  }

  &__props {
    width: 320px;
    flex: none;
    background: var(--td-bg-color-container);
    border: 1px solid var(--td-component-stroke);
    border-radius: 8px;
    overflow: hidden;
  }

  &__ghost {
    position: fixed;
    z-index: 3000;
    padding: 4px 10px;
    font-size: 12px;
    color: var(--td-text-color-anti);
    pointer-events: none;
    background: var(--app-brand-secondary);
    border-radius: 4px;
    transform: translate(-50%, -50%);
    opacity: 0.9;
  }
}

.palette-item {
  display: flex;
  gap: 8px;
  align-items: center;
  padding: 8px;
  cursor: grab;
  border: 1px solid var(--td-component-stroke);
  border-radius: 6px;
  transition: background 0.15s ease;
  // 拖拽源必须禁掉浏览器的手势接管（触摸板 / 触屏上默认会把手势判成「滚动面板」，
  // 触发 pointercancel 把拖拽掐断 —— 表现为「第一次能拖，第二次拖不动」）
  touch-action: none;
  user-select: none;

  &:hover {
    background: var(--td-bg-color-container-hover);
  }

  &.is-disabled {
    cursor: not-allowed;
    opacity: 0.5;
  }

  &__shape {
    width: 14px;
    height: 14px;
    flex: none;
    background: var(--td-bg-color-container);
    border: 2px solid var(--palette-stroke, var(--td-brand-color));
    border-radius: 3px;
  }

  &--start &__shape,
  &--end &__shape {
    border-radius: 999px;
  }

  &--condition &__shape,
  &--parallel &__shape {
    border-radius: 2px;
    transform: rotate(45deg);
  }

  &--start {
    --palette-stroke: var(--td-success-color);
  }

  &--end {
    --palette-stroke: var(--td-text-color-disabled);
  }

  &--approval {
    --palette-stroke: var(--td-brand-color);
  }

  &--countersign {
    --palette-stroke: var(--app-brand-secondary);
  }

  &--condition {
    --palette-stroke: var(--td-warning-color);
  }

  &--parallel {
    --palette-stroke: color-mix(in srgb, var(--td-brand-color-7) 55%, var(--app-danger));
  }

  &--cc {
    --palette-stroke: var(--td-text-color-placeholder);
  }

  // P-FLOW-1C：任务节点与画布上的 `is-task` 同色系（同一套变量，避免两处漂移）；
  // 类名带下划线（`type.toLowerCase()` 得来），别写成 `--task-node`
  &--task_node {
    --palette-stroke: color-mix(in srgb, var(--td-brand-color) 62%, var(--td-success-color));
  }

  &--notify_node {
    --palette-stroke: color-mix(in srgb, var(--td-warning-color) 55%, var(--td-text-color-placeholder));
  }

  &__text {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }

  &__label {
    font-size: 13px;
    color: var(--td-text-color-primary);
  }

  &__hint {
    overflow: hidden;
    font-size: 11px;
    color: var(--td-text-color-placeholder);
    text-overflow: ellipsis;
    white-space: nowrap;
  }
}

.issue-board {
  position: absolute;
  bottom: 10px;
  left: 10px;
  width: min(560px, calc(100% - 20px));
  max-height: 40%;
  padding: 8px 10px;
  overflow-y: auto;
  background: var(--td-bg-color-container);
  border: 1px solid var(--td-error-color);
  border-radius: 6px;
  box-shadow: var(--td-shadow-2);

  &__head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding-bottom: 6px;
    font-size: 13px;
    font-weight: 600;
    color: var(--td-error-color);
  }

  &__icon {
    margin-right: 4px;
    color: var(--td-error-color);
  }

  &__list {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 0;
    margin: 0;
    list-style: none;
  }

  &__item {
    display: flex;
    gap: 8px;
    align-items: center;
    padding: 4px 6px;
    font-size: 12px;
    color: var(--td-text-color-primary);
    cursor: pointer;
    border-radius: 4px;

    &:hover {
      background: var(--td-bg-color-container-hover);
    }
  }

  &__text {
    flex: 1;
    min-width: 0;
  }

  &__locate {
    flex: none;
    font-size: 11px;
    color: var(--td-brand-color);
  }
}

.warning-list {
  padding-left: 18px;
  margin: 0;
  font-size: 13px;
  line-height: 1.8;
  color: var(--td-text-color-primary);
}
</style>
