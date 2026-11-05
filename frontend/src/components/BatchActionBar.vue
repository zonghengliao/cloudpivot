<!--
  批量操作工具栏（D7）：勾选 → 进度 → 结果（失败逐条列原因）。

  # 为什么做成一个组件

  八个列表页的「批量」行为必须一模一样（本页/全部筛选结果、进度 N/M、失败逐条、
  高危二次确认 + 影响条数），复制八份必然漂移。判据全在 `@/utils/batch-action`，
  本文件只负责画和串请求。

  # 交互约定

  | 场景 | 行为 |
  |------|------|
  | 没勾选 | 整条工具栏不出现（不占位、不显示灰按钮） |
  | 状态不符合 | 该按钮禁用 + tooltip 说明「哪些状态不符合」 |
  | 高危（作废/删除） | 弹二次确认，**正文带影响条数**；要原因的再给输入框 |
  | 执行中 | 进度条 N/M（前端按 20 条一批，批与批之间刷新） |
  | 结束 | 结果弹窗：成功 X / 失败 Y + 失败清单「单号：原因」，可整段复制 |
  | 结束后 | 清空勾选（**刷新不保持勾选**：避免误点第二次批量）；工具栏要一直留到结果弹窗被关掉—— 弹窗是根节点的子节点，`visible` 必须把 `resultVisible` 算进去，否则勾选一清空整个根节点就被 `v-if` 卸掉，结果永远看不到 |
  | 关掉结果弹窗 | 清掉本次 outcome（结果是一次性的，下次动作不许残留上一次的成功/失败清单） |

  # 事件

  - `update:selected`：父组件维护勾选（清空、全选筛选结果都走这里）
  - `done`：一批跑完（父组件据此刷新列表）
-->
<template>
  <div v-if="visible" class="batch-bar" :data-testid="testid">
    <div class="batch-bar__info">
      <span class="batch-bar__count" :data-testid="`${testid}-count`">{{ countText }}</span>
      <t-link
        v-if="canSelectAll"
        theme="primary"
        size="small"
        :data-testid="`${testid}-select-all`"
        @click="selectAllFiltered"
      >
        选中全部 {{ filteredTotal }} 条筛选结果
      </t-link>
      <t-tag
        v-if="scope === 'filtered'"
        size="small"
        theme="warning"
        variant="light"
        :data-testid="`${testid}-scope`"
      >
        全部筛选结果
      </t-tag>
      <t-link theme="default" size="small" :data-testid="`${testid}-clear`" @click="clearSelection">
        清空
      </t-link>
    </div>

    <div class="batch-bar__actions">
      <t-tooltip
        v-for="action in actions"
        :key="action.key"
        :content="tipOf(action)"
        :disabled="!tipOf(action)"
      >
        <t-button
          size="small"
          :theme="action.theme ?? 'default'"
          :variant="action.theme === 'danger' ? 'outline' : 'base'"
          :disabled="!!tipOf(action) || running"
          :data-testid="`${testid}-${action.key}`"
          @click="onAction(action)"
        >
          {{ action.label }}
        </t-button>
      </t-tooltip>
    </div>

    <div v-if="running" class="batch-bar__progress" :data-testid="`${testid}-progress`">
      <t-progress :percentage="percent" theme="line" :label="false" />
      <span class="batch-bar__progress-text" :data-testid="`${testid}-progress-text`">
        正在处理 {{ progressLabel }}
      </span>
    </div>

    <!-- 高危二次确认：影响条数写在正文里，要原因的再给输入框 -->
    <t-dialog
      v-model:visible="confirmVisible"
      :header="pendingAction?.label ?? '确认执行'"
      :confirm-btn="'确认执行'"
      :cancel-btn="'取消'"
      :width="520"
      :data-testid="`${testid}-confirm`"
      @confirm="confirmAndRun"
    >
      <p class="batch-bar__confirm" :data-testid="`${testid}-confirm-text`">
        {{ pendingAction ? confirmMessage(pendingAction, selected.length) : '' }}
      </p>
      <template v-if="pendingAction?.requireReason">
        <t-textarea
          v-model="reason"
          :autosize="{ minRows: 2, maxRows: 4 }"
          :maxlength="255"
          placeholder="请填写原因（会写入单据留痕，事后可追溯）"
          :data-testid="`${testid}-reason`"
        />
        <p v-if="reasonInvalid" class="batch-bar__error" :data-testid="`${testid}-reason-error`">
          {{ reasonInvalid }}
        </p>
      </template>
      <!-- 批量指派：必须选到具体某个人（空值不许提交） -->
      <template v-if="pendingAction?.requireOwner">
        <t-select
          v-model="ownerId"
          class="batch-bar__owner"
          :options="ownerOptions"
          filterable
          clearable
          placeholder="选择新负责人"
          :data-testid="`${testid}-owner`"
        />
        <p v-if="ownerInvalid" class="batch-bar__error" :data-testid="`${testid}-owner-error`">
          {{ ownerInvalid }}
        </p>
      </template>
    </t-dialog>

    <!-- 结果：成功 X / 失败 Y + 可复制的失败清单 -->
    <!-- 显隐自己接管（不用 v-model:visible）：关掉时要顺手清掉本次 outcome，见 onResultVisibleChange -->
    <t-dialog
      :visible="resultVisible"
      :header="headline"
      :footer="false"
      :width="640"
      :data-testid="`${testid}-result`"
      @update:visible="onResultVisibleChange"
    >
      <p class="batch-bar__hint" :data-testid="`${testid}-result-hint`">{{ hint }}</p>
      <div v-if="failures.length > 0" class="batch-bar__failures">
        <div class="batch-bar__failures-head">
          <span>失败 {{ failures.length }} 条（逐条原因）</span>
          <t-link theme="primary" size="small" :data-testid="`${testid}-copy`" @click="copyFailures">
            复制清单
          </t-link>
        </div>
        <ul class="batch-bar__failure-list" :data-testid="`${testid}-failure-list`">
          <li v-for="(line, index) in failures" :key="index">{{ line }}</li>
        </ul>
      </div>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import {
  BATCH_CHUNK,
  actionDisabledReason,
  chunk,
  confirmMessage,
  failureLines,
  isExportTask,
  mergeBatchResults,
  progressPercent,
  progressText,
  reasonError,
  ownerError,
  resultHeadline,
  resultHint,
  selectionText,
  type BatchActionSpec,
  type BatchExportTask,
  type BatchOutcome,
  type BatchResult,
  type BatchRow,
  type BatchRunner,
} from '@/utils/batch-action';

const props = withDefaults(
  defineProps<{
    /** 勾选的行（父组件维护）。 */
    selected: BatchRow[];
    /** 可用的批量动作。 */
    actions: BatchActionSpec[];
    /** 执行器：一个 chunk 一次调用（页面里就是「调一个批量接口」）。 */
    runner: BatchRunner;
    /** 当前筛选结果总条数（用于「本页 / 全部」的提示）。 */
    filteredTotal?: number;
    /** 「选中全部筛选结果」时怎么把全部行拿回来；不传就不显示该链接。 */
    fetchAllIds?: () => Promise<BatchRow[]>;
    /** 单条动作的权限判定；不传 = 都有权限。 */
    can?: (action: BatchActionSpec) => boolean;
    /** 批量指派的可选用户（`requireOwner` 的动作用它渲染下拉）。 */
    ownerOptions?: Array<{ label: string; value: string }>;
    /** testid 前缀。 */
    testid?: string;
  }>(),
  {
    filteredTotal: 0,
    fetchAllIds: undefined,
    can: () => true,
    ownerOptions: () => [],
    testid: 'batch-bar',
  }
);

const emit = defineEmits<{
  (event: 'update:selected', rows: BatchRow[]): void;
  (event: 'done', outcome: BatchOutcome): void;
}>();

/** 勾选范围：本页 / 全部筛选结果（**必须让用户看得见**，卡面 §三 ②）。 */
const scope = ref<'page' | 'filtered'>('page');
const running = ref(false);
const done = ref(0);
const total = ref(0);
const confirmVisible = ref(false);
const pendingAction = ref<BatchActionSpec>();
const reason = ref('');
const ownerId = ref('');
const resultVisible = ref(false);
const outcome = ref<BatchOutcome>();

/**
 * 整条工具栏（含两个弹窗）的挂载判据。
 *
 * **必须把 `resultVisible` 也算进来**：`execute()` 成功收尾会
 * `resultVisible = true` 然后 `emit('update:selected', [])` 清空勾选，
 * 只判「有勾选 / 正在跑」的话这里立刻变 false → 根节点 `v-if` 把结果弹窗
 * 一起卸掉，用户永远看不到「成功 X / 失败 Y」（t_cea00ecf 的根因）。
 * 结果弹窗是根节点的子节点，所以「结果还没被用户关掉」等于「根节点必须还在」。
 * 关掉弹窗后 `resultVisible=false` 且勾选为空 → 工具栏照常隐藏。
 */
const visible = computed(
  () => props.selected.length > 0 || running.value || resultVisible.value
);
const countText = computed(() =>
  selectionText(props.selected.length, scope.value, props.filteredTotal)
);
const canSelectAll = computed(
  () =>
    typeof props.fetchAllIds === 'function' &&
    props.filteredTotal > props.selected.length &&
    scope.value === 'page'
);
const progressLabel = computed(() => progressText(done.value, total.value));
const percent = computed(() => progressPercent(done.value, total.value));
const reasonInvalid = computed(() =>
  pendingAction.value ? reasonError(pendingAction.value, reason.value) : undefined
);
const ownerInvalid = computed(() =>
  pendingAction.value ? ownerError(pendingAction.value, ownerId.value) : undefined
);
const headline = computed(() => (outcome.value ? resultHeadline(outcome.value) : '批量操作结果'));
const hint = computed(() => (outcome.value ? resultHint(outcome.value) : ''));
const failures = computed(() => (outcome.value ? failureLines(outcome.value) : []));

/** 按钮提示：禁用原因（空 = 可点）。 */
function tipOf(action: BatchActionSpec): string | undefined {
  if (running.value) {
    return '正在处理，请稍候';
  }
  return actionDisabledReason(action, props.selected, props.can(action));
}

/** 点按钮：高危先确认，其余直接跑。 */
function onAction(action: BatchActionSpec): void {
  if (tipOf(action)) {
    return;
  }
  if (action.needConfirm || action.requireReason || action.requireOwner) {
    pendingAction.value = action;
    reason.value = '';
    ownerId.value = '';
    confirmVisible.value = true;
    return;
  }
  void execute(action, undefined);
}

/** 确认框「确认执行」。 */
function confirmAndRun(): void {
  const action = pendingAction.value;
  if (!action) {
    return;
  }
  if (reasonError(action, reason.value) || ownerError(action, ownerId.value)) {
    return;
  }
  confirmVisible.value = false;
  void execute(
    action,
    reason.value.trim() === '' ? undefined : reason.value.trim(),
    ownerId.value.trim() === '' ? undefined : ownerId.value.trim()
  );
}

/** 真正执行：按 20 条一批，批与批之间刷新进度。 */
async function execute(
  action: BatchActionSpec,
  reasonText?: string,
  owner?: string
): Promise<void> {
  const ids = props.selected.map((row) => row.id);
  const parts = chunk(ids, BATCH_CHUNK);
  running.value = true;
  done.value = 0;
  total.value = ids.length;
  const results: BatchResult[] = [];
  let exportTask: BatchExportTask | undefined;
  try {
    for (const part of parts) {
      const value = await props.runner(action, part, { reason: reasonText, ownerId: owner });
      if (isExportTask(value)) {
        exportTask = value;
      } else {
        results.push(value);
      }
      done.value += part.length;
    }
  } catch (error) {
    running.value = false;
    const message = error instanceof Error ? error.message : String(error);
    MessagePlugin.error(`批量操作未执行：${message}`);
    return;
  }
  running.value = false;
  const finished: BatchOutcome = exportTask
    ? { kind: 'export', task: exportTask }
    : { kind: 'batch', result: mergeBatchResults(results) };
  outcome.value = finished;
  resultVisible.value = true;
  emit('done', finished);
  // 刷新后不保持勾选（卡面 §三 ⑤）：避免用户以为还要再点一次
  emit('update:selected', []);
  scope.value = 'page';
}

/** 清空勾选。 */
function clearSelection(): void {
  emit('update:selected', []);
  scope.value = 'page';
}

/**
 * 结果弹窗的显隐回写（用户点右上角 × / 遮罩 / Esc 都会走到这里）。
 *
 * 关掉时**同步清掉 `outcome`**：结果是一次性的，留着会让下一次批量动作
 * （或页面复用同一实例）在弹窗刚打开的那一帧闪出上一次的成功/失败清单。
 * 清空不影响关闭动画——勾选此时已清空，`visible=false` 会让整条工具栏
 * 连同弹窗一起消失（没有残留的淡出画面）。
 */
function onResultVisibleChange(value: boolean): void {
  resultVisible.value = value;
  if (!value) {
    outcome.value = undefined;
  }
}

/** 选中全部筛选结果（父组件负责取全部行）。 */
async function selectAllFiltered(): Promise<void> {
  if (!props.fetchAllIds) {
    return;
  }
  try {
    const rows = await props.fetchAllIds();
    if (rows.length === 0) {
      MessagePlugin.warning('当前筛选条件下没有可勾选的行');
      return;
    }
    scope.value = 'filtered';
    emit('update:selected', rows);
  } catch (error) {
    const message = error instanceof Error ? error.message : String(error);
    MessagePlugin.error(`读取筛选结果失败：${message}`);
  }
}

/** 复制失败清单（贴给同事 / 工单）。 */
async function copyFailures(): Promise<void> {
  if (!outcome.value) {
    return;
  }
  const text = failures.value.join('\n');
  if (!text) {
    return;
  }
  try {
    await navigator.clipboard.writeText(text);
    MessagePlugin.success(`已复制 ${failures.value.length} 条失败原因`);
  } catch {
    MessagePlugin.warning('浏览器不允许剪贴板访问：请手动选中清单复制');
  }
}
</script>

<style scoped>
.batch-bar {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 12px;
  padding: 10px 12px;
  margin: 8px 0;
  border: 1px solid var(--td-brand-color-light);
  border-radius: 6px;
  background: var(--td-brand-color-light, #eef3ff);
}

.batch-bar__info {
  display: flex;
  align-items: center;
  gap: 12px;
}

.batch-bar__count {
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.batch-bar__actions {
  display: flex;
  gap: 8px;
}

.batch-bar__progress {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 220px;
  flex: 1;
}

.batch-bar__progress-text {
  white-space: nowrap;
  color: var(--td-text-color-secondary);
}

.batch-bar__confirm {
  margin: 0;
  line-height: 1.7;
}

.batch-bar__error {
  margin: 6px 0 0;
  color: var(--td-error-color);
  font-size: 12px;
}

.batch-bar__hint {
  margin: 0 0 8px;
  color: var(--td-text-color-secondary);
}

.batch-bar__failures {
  border-top: 1px dashed var(--td-component-stroke);
  padding-top: 8px;
}

.batch-bar__failures-head {
  display: flex;
  justify-content: space-between;
  align-items: center;
  margin-bottom: 6px;
  font-weight: 600;
}

.batch-bar__failure-list {
  max-height: 240px;
  overflow-y: auto;
  margin: 0;
  padding-left: 18px;
  line-height: 1.7;
  color: var(--td-error-color);
}
</style>
