<template>
  <div class="task-preview" data-testid="task-node-preview">
    <div class="task-preview__head">
      <span class="task-preview__title">
        <t-icon name="browse" />
        实时预览
        <t-tag v-if="loading" size="small" variant="light" theme="default">更新中…</t-tag>
      </span>
      <span class="task-preview__ops">
        <label class="task-preview__strict">
          <t-switch v-model="strict" size="small" />
          <span>严格模式</span>
        </label>
        <t-link theme="primary" size="small" @click="run(true)">立即刷新</t-link>
      </span>
    </div>

    <!-- 决策 5：示例数据必须**顶部黄条**写清，而且不许写进任何持久化字段 -->
    <t-alert theme="warning" class="task-preview__sample" message="示例数据，非真实单据">
      <template #description>
        预览用的是下面这组**前端硬编码**的示例变量，不会写进流程定义、实例或任务：
        <code>{{ sampleSummary }}</code>
      </template>
    </t-alert>

    <!-- 没有标题模板时后端一定 65034，没必要打请求 -->
    <p v-if="!hasTitle" class="task-preview__placeholder">先填「任务标题」再看预览。</p>

    <t-loading v-else :loading="loading" size="small">
      <!-- 错误清单（红）：后端 400 + 65034~65039 的原文，或前端预检结论 -->
      <div v-if="errors.length > 0" class="task-preview__block task-preview__block--error" data-testid="task-preview-errors">
        <div class="task-preview__block-title">
          <t-icon name="error-circle" />
          {{ strict ? '严格模式下的错误' : '错误' }}（{{ errors.length }}）
          <t-tag v-if="errorCode" size="small" variant="light" theme="danger">{{ errorCode }}</t-tag>
        </div>
        <ul>
          <li v-for="(item, index) in errors" :key="index">{{ item }}</li>
        </ul>
        <p v-if="!strict" class="task-preview__block-hint">
          打开「严格模式」看运行期会怎么报；关掉时缺变量会渲染成可见占位 〔form.xxx〕。
        </p>
      </div>

      <!-- 告警清单（黄）：缺变量的可见占位、固定日期已过期、解析出 0 人… -->
      <div v-if="warnings.length > 0" class="task-preview__block task-preview__block--warn" data-testid="task-preview-warnings">
        <div class="task-preview__block-title">
          <t-icon name="info-circle" />
          需要注意（{{ warnings.length }}）
        </div>
        <ul>
          <li v-for="(item, index) in warnings" :key="index">{{ item }}</li>
        </ul>
      </div>

      <div v-if="result?.task" class="task-preview__task">
        <div class="task-preview__row">
          <span class="task-preview__label">标题</span>
          <span class="task-preview__value task-preview__value--title">{{ result.task.title }}</span>
        </div>
        <div v-if="result.task.description" class="task-preview__row">
          <span class="task-preview__label">描述</span>
          <pre class="task-preview__desc">{{ result.task.description }}</pre>
        </div>
        <div class="task-preview__row">
          <span class="task-preview__label">优先级</span>
          <span class="task-preview__value">{{ result.task.priority }}</span>
          <span class="task-preview__label task-preview__label--inline">期限</span>
          <span class="task-preview__value">{{ result.task.due_date ?? '不设期限' }}</span>
        </div>
        <div class="task-preview__row">
          <span class="task-preview__label">完成方式</span>
          <span class="task-preview__value">{{ onCompleteText }}</span>
          <t-tag v-if="result.task.need_review" size="small" variant="outline" theme="default">需验收</t-tag>
        </div>
        <div class="task-preview__row">
          <span class="task-preview__label">负责人</span>
          <span v-if="!assigneeConfigured" class="task-preview__value task-preview__value--muted">未配置指派来源</span>
          <span v-else-if="assignees.length === 0" class="task-preview__value task-preview__value--muted">
            解析出 0 人（运行期按「空审批人」策略处理）
          </span>
          <span v-else class="task-preview__value">
            {{ assignees[0]?.name }}（{{ assignees[0]?.source }}）
            <template v-if="assignees.length > 1">
              ，协作人 {{ assignees.length - 1 }} 人
            </template>
          </span>
        </div>
        <details v-if="result.trace.length > 0" class="task-preview__trace">
          <summary>排障痕迹（{{ result.trace.length }}）</summary>
          <ul>
            <li v-for="(item, index) in result.trace" :key="index">{{ item }}</li>
          </ul>
        </details>
      </div>

      <p v-else-if="errors.length === 0" class="task-preview__placeholder">{{ emptyText }}</p>
    </t-loading>
  </div>
</template>

<script setup lang="ts">
/**
 * 任务节点实时预览（设计 §4.4 的 **L1 单节点预演**）。
 *
 * # 只做 L1，不做 L3（决策 5 / 设计 §9.4）
 *
 * 预览只回答三件事：**模板能渲染、期限能算出、负责人能解析**。
 * 它**不**保证图结构合法（那是 L2 全图静态校验），也**不**建实例、不生成任务、不发通知
 * （沙箱试跑明确不做 —— 那需要第二套运行路径，收益只是「提前看一眼」）。
 *
 * # 防抖 300ms
 *
 * 属性面板上每敲一个字都发一次预演请求是灾难（后端要查角色、算工作日）。
 * 改完 300ms 内没有新输入才发 —— 与「列表记忆 300ms 防抖落盘」同一节奏。
 *
 * # 空态与缺变量（决策 5 + 设计 §4.4「L1 预览的空态设计」）
 *
 * - 变量永远用**前端硬编码的示例数据**（流程定义刚建时没有任何真实单据）；
 * - 顶部**黄条**明写「示例数据，非真实单据」，并列出用了哪些示例值；
 * - 缺变量由后端渲染成可见占位 `〔form.xxx〕` 并进 `warnings`（黄），
 *   **不是**静默替换空串，也**不许**出现「已自动填充默认值」这种假安慰；
 * - 切「严格模式」= 与运行期完全一致：缺变量直接 `400 + 65039` → 红色错误清单。
 */
import { computed, onBeforeUnmount, ref, watch } from 'vue';
import { ApiError } from '@/api/request';
import { previewTaskNode, type PreviewAssigneeRes, type TaskPreviewAssigneeReq, type TaskPreviewRes } from '@/api/business/flow-api';
import { taskNodeErrorText } from '@/views/business/flow/components/flow-error-codes';
import { PO_APPROVE_BIZ_TYPE } from '@/views/business/flow/flow-business-items';
import {
  ON_COMPLETE_LABELS,
  taskConfigPatch,
  type OnComplete,
  type TaskConfig,
} from '@/views/business/flow/components/task-node-config';

/** 组件入参 */
const props = withDefaults(
  defineProps<{
    /** 面板上正在编辑的 `config.task`（不用等保存） */
    config: TaskConfig;
    /** 本流程业务类型（决定 `task.*` 是否可用） */
    bizType?: string | null;
    /** 节点名（`{{node.name}}` 的取值） */
    nodeName?: string | null;
    /** 指派配置（面板上那几个可编辑项；给 null = 只演模板） */
    assignee?: TaskPreviewAssigneeReq | null;
    /** 假设今天是哪天（`yyyy-MM-dd`，固定住让「3 个工作日」的结果可复现） */
    dueBase?: string | null;
  }>(),
  { bizType: null, nodeName: null, assignee: null, dueBase: null },
);

/** 防抖间隔（卡面验收 4：改标题模板 300ms 后更新） */
const DEBOUNCE_MS = 300;

/**
 * 示例变量（**前端硬编码，只在这里**）。
 *
 * 取值照设计 §4.4 的样例（`PO202611010001` / `125000` / `张三` / `李四`），
 * `initiator.id` 与 `prev.approver_id` 用固定的全零 UUID —— 后端 `assignee::resolve`
 * 会把它们当 uuid 解析，给个非 uuid 字符串会在「发起人本人」来源上 400。
 */
const SAMPLE_VARS = {
  // biz_type 取词表里的真值：采购单走的是「采购订单审批」，值 = PO_APPROVE
  // （旧版这里硬编码 'PURCHASE_ORDER' —— 那是**实体类型**不是审批类型，
  //  与 biz_flow_def.biz_type 不是同一根轴，预览会显示「本流程业务类型对不上」）。
  instance: {
    biz_no: 'PO202611010001',
    title: '采购申请（示例）',
    biz_type: PO_APPROVE_BIZ_TYPE as string,
  },
  initiator: { id: '00000000-0000-0000-0000-000000000001', name: '张三', dept_id: '00000000-0000-0000-0000-000000000002' },
  prev: { approver_name: '李四', approver_id: '00000000-0000-0000-0000-000000000003' },
  form: { supplier_name: '华东金属', amount: 125000, delivery_date: '2026-11-15', urgency: 'P1', task_owner_id: '00000000-0000-0000-0000-000000000001' },
} as const;

/** 严格模式（与运行期一致：缺变量直接报错） */
const strict = ref(false);
/** 加载中 */
const loading = ref(false);
/** 最近一次成功回执 */
const result = ref<TaskPreviewRes | null>(null);
/** 最近一次错误的文案（逐条） */
const errors = ref<string[]>([]);
/** 最近一次错误的码（徽标用） */
const errorCode = ref<number | null>(null);
/**
 * 告警的覆盖值（`null` = 用后端回执里的 `warnings`）。
 *
 * 只在「负责人解析失败 → 降级重演」时用：那一次的 65018 原文要挂进黄色清单，
 * 但后端回执里没有它（它是上一次请求的错误）。
 */
const warningsOverride = ref<string[] | null>(null);
/** 本组件是否已卸载（防止卸载后回写状态） */
let disposed = false;
/** 防抖计时器 */
let timer: ReturnType<typeof setTimeout> | null = null;
/** 请求序号（后发先至时丢弃旧响应） */
let seq = 0;

/** 标题模板是否已填（空标题后端必 65034，不发请求） */
const hasTitle = computed(() => (props.config.title_template ?? '').trim().length > 0);

/** 是否配了指派来源（没配就只回模板结果，负责人那行显示「未配置」） */
const assigneeConfigured = computed(() => Boolean(props.assignee?.assignee_type));

/** 解析出的人 */
const assignees = computed<PreviewAssigneeRes[]>(() => result.value?.assignees ?? []);

/** 告警（黄条清单） */
const warnings = computed<string[]>(() => warningsOverride.value ?? result.value?.warnings ?? []);

/** 完成方式的中文（术语锁定：验收 = need_review，审批 = on_complete=APPROVE） */
const onCompleteText = computed(() => {
  const value = (result.value?.task?.on_complete ?? props.config.on_complete) as OnComplete;
  return ON_COMPLETE_LABELS[value] ?? value;
});

/** 示例变量的可读摘要（黄条里如实列出） */
const sampleSummary = computed(() =>
  Object.entries(SAMPLE_VARS)
    .flatMap(([root, leaves]) => Object.entries(leaves).map(([key, value]) => `${root}.${key}=${String(value)}`))
    .join(' · '),
);

/** 空态文案 */
const emptyText = computed(() => (loading.value ? '正在预演…' : '暂无预览结果'));

/** 假设的「今天」（默认今天零点，让「3 个工作日」这类结果稳定） */
function dueBaseOf(): string | null {
  if (props.dueBase) {
    return props.dueBase;
  }
  const now = new Date();
  return `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, '0')}-${String(now.getDate()).padStart(2, '0')}T00:00:00+08:00`;
}

/**
 * 发一次预演。
 *
 * # 参数
 * - `immediate`：跳过防抖（手动点「立即刷新」用）
 */
async function run(immediate = false): Promise<void> {
  if (timer) {
    clearTimeout(timer);
    timer = null;
  }
  if (!hasTitle.value) {
    result.value = null;
    errors.value = [];
    errorCode.value = null;
    return;
  }
  const start = () => {
    void request();
  };
  if (immediate) {
    start();
    return;
  }
  timer = setTimeout(start, DEBOUNCE_MS);
}

/** 真正打请求（带请求序号，避免后发先至覆盖） */
async function request(): Promise<void> {
  seq += 1;
  const mine = seq;
  loading.value = true;
  // 每次新请求都清掉上一轮「降级预演」的告警覆盖，避免旧提示挂在新结果上
  warningsOverride.value = null;
  try {
    const vars = JSON.parse(JSON.stringify(SAMPLE_VARS)) as Record<string, unknown>;
    // 示例里的 instance.biz_type 用真实流程的 biz_type（否则「本流程业务类型」的文案会对不上）
    (vars.instance as Record<string, unknown>).biz_type = props.bizType ?? PO_APPROVE_BIZ_TYPE;
    const response = await previewTaskNode({
      config: taskConfigPatch(props.config).config.task,
      assignee: props.assignee ?? null,
      vars,
      biz_type: props.bizType ?? null,
      node_name: props.nodeName ?? null,
      due_base: dueBaseOf(),
      strict: strict.value,
    });
    if (disposed || mine !== seq) {
      return;
    }
    result.value = response;
    errors.value = response.errors ?? [];
    errorCode.value = errors.value.length > 0 ? 65039 : null;
  } catch (error) {
    if (disposed || mine !== seq) {
      return;
    }
    const code = error instanceof ApiError ? error.code : 0;
    const message = error instanceof ApiError ? error.message : '预演请求失败（网络异常）';
    const text = taskNodeErrorText(code, message);
    // 65018 = 解析不出负责人。**示例数据里没有真实用户**，绝大多数「发起人 / 角色」来源都会命中它，
    // 但它不代表模板配错了 —— 所以：① 提示降级成黄色告警 ② 不带 assignee 再演一次，把模板结果照样给用户看。
    if (code === 65018) {
      await rerenderWithoutAssignee(text.text);
      return;
    }
    errors.value = [text.text];
    errorCode.value = code || null;
    result.value = null;
  } finally {
    if (!disposed && mine === seq) {
      loading.value = false;
    }
  }
}

/**
 * 负责人解析失败时的降级预演：**不带 `assignee`** 再打一次。
 *
 * 目的只有一个 —— 让用户看到「模板渲染 + 期限」这两件事是好的（L1 的三件保证里丢一件，另两件照给），
 * 同时把 65018 的原文放进黄色告警（不是红色错误：模板没配错）。
 */
async function rerenderWithoutAssignee(reason: string): Promise<void> {
  seq += 1;
  const mine = seq;
  try {
    const vars = JSON.parse(JSON.stringify(SAMPLE_VARS)) as Record<string, unknown>;
    (vars.instance as Record<string, unknown>).biz_type = props.bizType ?? PO_APPROVE_BIZ_TYPE;
    const response = await previewTaskNode({
      config: taskConfigPatch(props.config).config.task,
      assignee: null,
      vars,
      biz_type: props.bizType ?? null,
      node_name: props.nodeName ?? null,
      due_base: dueBaseOf(),
      strict: strict.value,
    });
    if (disposed || mine !== seq) {
      return;
    }
    result.value = response;
    errors.value = [];
    errorCode.value = null;
    warningsOverride.value = [
      `示例数据解析不出负责人：${reason}`,
      ...(response.warnings ?? []),
    ];
  } catch (error) {
    if (disposed || mine !== seq) {
      return;
    }
    const code = error instanceof ApiError ? error.code : 0;
    const message = error instanceof ApiError ? error.message : '预演请求失败（网络异常）';
    errors.value = [taskNodeErrorText(code, message).text];
    errorCode.value = code || null;
    result.value = null;
  }
}

/** 配置任何一项变了都要重演（含 due / need_review / on_complete 的联动结果） */
watch(
  () => [
    JSON.stringify(taskConfigPatch(props.config).config.task),
    JSON.stringify(props.assignee ?? null),
    props.bizType ?? '',
    props.nodeName ?? '',
    strict.value,
  ],
  () => run(),
  { immediate: true },
);

onBeforeUnmount(() => {
  disposed = true;
  if (timer) {
    clearTimeout(timer);
    timer = null;
  }
});
</script>

<style scoped lang="less">
.task-preview {
  display: flex;
  flex-direction: column;
  gap: 8px;
  padding-top: 10px;
  border-top: 1px solid var(--td-component-stroke);

  &__head {
    display: flex;
    gap: 8px;
    align-items: center;
    justify-content: space-between;
  }

  &__title {
    display: inline-flex;
    gap: 6px;
    align-items: center;
    font-size: 13px;
    font-weight: 600;
    color: var(--td-text-color-primary);
  }

  &__ops {
    display: inline-flex;
    gap: 10px;
    align-items: center;
  }

  &__strict {
    display: inline-flex;
    gap: 4px;
    align-items: center;
    font-size: 12px;
    color: var(--td-text-color-secondary);
    cursor: pointer;
  }

  &__sample {
    :deep(.t-alert__description) {
      font-size: 11px;
      line-height: 1.6;
      word-break: break-all;
    }
  }

  &__block {
    padding: 6px 8px;
    font-size: 12px;
    line-height: 1.6;
    border-radius: var(--td-radius-default);

    ul {
      padding-left: 16px;
      margin: 4px 0 0;
    }

    &--error {
      color: var(--td-error-color);
      background: var(--td-error-color-1);
    }

    &--warn {
      color: var(--td-warning-color);
      background: var(--td-warning-color-1);
    }
  }

  &__block-title {
    display: flex;
    gap: 6px;
    align-items: center;
    font-weight: 600;
  }

  &__block-hint {
    margin: 4px 0 0;
    font-size: 11px;
    line-height: 1.5;
    color: var(--td-text-color-placeholder);
  }

  &__task {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 8px 10px;
    background: var(--td-bg-color-container);
    border: 1px solid var(--td-component-stroke);
    border-radius: var(--td-radius-default);
  }

  &__row {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: baseline;
    font-size: 12px;
  }

  &__label {
    flex: none;
    color: var(--td-text-color-placeholder);

    &--inline {
      margin-left: 10px;
    }
  }

  &__value {
    min-width: 0;
    color: var(--td-text-color-primary);
    word-break: break-all;

    &--title {
      font-weight: 600;
    }

    &--muted {
      color: var(--td-text-color-placeholder);
    }
  }

  &__desc {
    width: 100%;
    margin: 0;
    font-family: inherit;
    font-size: 12px;
    line-height: 1.6;
    white-space: pre-wrap;
    word-break: break-all;
  }

  &__trace {
    font-size: 11px;
    color: var(--td-text-color-placeholder);

    summary {
      cursor: pointer;
    }

    ul {
      padding-left: 16px;
      margin: 4px 0 0;
      line-height: 1.6;
    }
  }

  &__placeholder {
    margin: 0;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }
}
</style>
