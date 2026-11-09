<template>
  <t-drawer
    v-model:visible="innerVisible"
    header="状态机边配置"
    size="1000px"
    :footer="false"
    data-testid="state-edge-drawer"
  >
    <div class="se">
      <t-alert theme="info" :close="false" class="se__tip">
        <template #message>
          这里显示的是<strong>代码里的 BASE 边</strong>（<code>state_machine::BASE_EDGES</code>）与库里的配置合并后的结果。
          改任何一条都要提交 <strong>flow_edge_config 审批</strong>：审批通过才写库，驳回或撤回配置一分不动。
        </template>
      </t-alert>

      <div class="se__bar">
        <t-select
          v-model="currentEntity"
          class="se__entity"
          :options="entityOptions"
          placeholder="选择实体"
          data-testid="state-edge-entity"
        />
        <t-button variant="outline" size="small" :loading="loading" data-testid="state-edge-refresh" @click="load">
          <template #icon><t-icon name="refresh" /></template>
          刷新
        </t-button>
        <span class="se__meta" data-testid="state-edge-meta">
          BASE 边 {{ rows.length }} 条 · 生效 {{ effectiveCount }} 条 · 存活配置 {{ currentView?.configs.length ?? 0 }} 条
        </span>
      </div>

      <t-alert v-if="errorMessage" theme="error" class="se__alert" :message="errorMessage">
        <template #operation>
          <t-button size="small" variant="outline" @click="load">重试</t-button>
        </template>
      </t-alert>

      <t-table
        row-key="key"
        :data="rows"
        :columns="columns"
        :loading="loading"
        size="small"
        :hover="true"
        data-testid="state-edge-table"
      >
        <template #from="{ row }">
          <span class="mono">{{ row.from }}</span>
        </template>
        <template #to="{ row }">
          <span class="mono">{{ row.to }}</span>
        </template>
        <template #label="{ row }">
          <span>{{ row.label }}</span>
        </template>
        <template #mandatory="{ row }">
          <t-tag size="small" variant="light" :theme="row.mandatoryTheme" data-testid="state-edge-mandatory">
            {{ row.mandatoryText }}
          </t-tag>
        </template>
        <template #critical="{ row }">
          <t-tag v-if="row.critical" size="small" theme="danger" variant="light" data-testid="state-edge-critical">
            主链路
          </t-tag>
          <span v-else class="muted">—</span>
        </template>
        <template #reason="{ row }">
          <t-tag v-if="row.reasonText === '必须填原因'" size="small" variant="light" theme="warning">必须填原因</t-tag>
          <span v-else class="muted">不要求原因</span>
        </template>
        <template #auth_code="{ row }">
          <span :class="row.authCodeText === '—' ? 'muted' : 'mono'">{{ row.authCodeText }}</span>
        </template>
        <template #configured="{ row }">
          <t-tag size="small" variant="outline" :theme="row.configuredText === '已配置' ? 'primary' : 'default'">
            {{ row.configuredText }}
          </t-tag>
        </template>
        <template #enabled="{ row }">
          <t-tag size="small" :theme="row.enabledTheme" data-testid="state-edge-enabled">
            {{ row.enabledText }}
          </t-tag>
        </template>
        <template #op="{ row }">
          <t-space size="small">
            <t-link
              v-privilege="[STATE_EDGE_PERMISSIONS.manage]"
              theme="primary"
              data-testid="state-edge-change"
              @click="openChange(row)"
            >
              变更
            </t-link>
            <t-link
              v-if="row.configuredText === '已配置'"
              v-privilege="[STATE_EDGE_PERMISSIONS.manage]"
              theme="danger"
              @click="openDelete(row)"
            >
              删除配置
            </t-link>
          </t-space>
        </template>
        <template #empty>
          <div class="se__empty">
            <p>这个实体没有 BASE 边（代码里没定义任何状态流转）。换一个实体看看。</p>
          </div>
        </template>
      </t-table>

      <div v-if="pending.length" class="se__pending" data-testid="state-edge-pending">
        <div class="se__pending-title">在跑审批（{{ pending.length }}）——审批通过后才会生效</div>
        <ul>
          <li v-for="item in pending" :key="item.instance_id" data-testid="state-edge-pending-item">
            <t-tag size="small" variant="light" theme="warning">{{ item.action }}</t-tag>
            <span class="mono">{{ pendingChangeText(item) }}</span>
          </li>
        </ul>
      </div>
    </div>
  </t-drawer>

  <!-- 变更 / 删除：一律提交审批，不直写 -->
  <t-dialog
    v-model:visible="dialogVisible"
    :header="dialogMode === 'DELETE' ? '删除边配置（提交审批）' : '变更边配置（提交审批）'"
    width="600px"
    :confirm-btn="{
      content: dialogMode === 'DELETE' ? '提交删除审批' : '提交审批',
      theme: dialogMode === 'DELETE' ? 'danger' : 'primary',
      loading: submitting,
    }"
    data-testid="state-edge-change-dialog"
    @confirm="submitChange"
  >
    <div v-if="target" class="ce">
      <div class="ce__edge" data-testid="state-edge-change-target">
        <span class="mono">{{ target.edge.from }} → {{ target.edge.to }}</span>
        <span class="ce__label">{{ target.edge.label }}</span>
        <t-tag size="small" variant="light" :theme="target.mandatoryTheme">{{ target.mandatoryText }}</t-tag>
        <t-tag v-if="target.critical" size="small" theme="danger" variant="light">主链路</t-tag>
      </div>

      <!-- 【E2E-FIX-1】禁用主链路必需边：红色警告 + 强制勾选（后端还有 confirm_critical 硬门） -->
      <template v-if="showCriticalWarning">
        <t-alert
          theme="error"
          :title="`禁用主链路必需边：${criticalImpactText(target.edge)}`"
          data-testid="state-edge-critical-warning"
        >
          <template #message>
            这是全公司业务链上的必需边，关掉之后
            <strong>{{ criticalImpactText(target.edge) }}</strong>，且运行时不报「配置问题」只报「找管理员」——
            业务用户当场就卡住，只能等系统管理员改配置。
            如果只是想临时加严流程，请改为<strong>启用</strong>并调整审批设置，不要禁用这条边。
          </template>
        </t-alert>
        <t-checkbox v-model="criticalConfirmed" data-testid="state-edge-critical-confirm">
          我确认要禁用主链路必需边
        </t-checkbox>
      </template>

      <template v-if="dialogMode === 'UPSERT'">
        <t-form label-align="top">
          <t-form-item label="是否启用">
            <t-switch v-model="changeForm.enabled" :disabled="!target.canDisable" data-testid="state-edge-form-enabled" />
            <span class="form-tip">
              {{ target.canDisable ? '关掉 = 禁用这条可选边（运行时按钮不再出现）' : '硬约束边（mandatory）禁不掉 —— 内核拒绝，开关已锁死' }}
            </span>
          </t-form-item>
          <t-form-item label="是否必须填原因">
            <t-switch
              v-model="changeForm.requires_reason"
              :disabled="target.reasonLocked"
              data-testid="state-edge-form-reason"
            />
            <span class="form-tip">
              {{ target.reasonLocked ? 'BASE 边已要求原因，配置只能加严不能放宽（已锁死）' : '打开 = 走这条边必须填原因' }}
            </span>
          </t-form-item>
          <t-form-item label="显示文案">
            <t-input v-model="changeForm.label" :maxlength="64" placeholder="留空 = 用 BASE 默认文案" />
          </t-form-item>
          <t-form-item label="追加权限码">
            <t-input v-model="changeForm.auth_code" :maxlength="128" placeholder="留空 = 不追加（如 business:erp:stock-out:post）" />
          </t-form-item>
        </t-form>
      </template>

      <t-form label-align="top">
        <t-form-item :label="dialogMode === 'DELETE' ? '删除理由（写进审批单）' : '变更理由（写进审批单）'">
          <t-textarea
            v-model="changeForm.reason"
            :autosize="{ minRows: 2, maxRows: 5 }"
            :maxlength="200"
            placeholder="为什么要改？复核人靠它判断该不该批"
            data-testid="state-edge-form-reason-text"
          />
        </t-form-item>
      </t-form>

      <div class="ce__summary" data-testid="state-edge-change-summary">
        提交后：{{ dialogMode === 'DELETE' ? '删除配置，回到 BASE 行为' : changeSummary(target.edge, changeForm) }}
      </div>
      <p class="ce__hint">
        {{ dialogMode === 'DELETE' ? '删除同样走审批' : '注意' }}：点「{{ dialogMode === 'DELETE' ? '提交删除审批' : '提交审批' }}」
        只会生成一张 <strong>flow_edge_config</strong> 审批单，<strong>不会立刻改库</strong>；
        审批通过后由回调写 <code>biz_flow_state_edge</code>。
      </p>
    </div>
  </t-dialog>
</template>

<script setup lang="ts">
/**
 * 状态机边配置面板（卡 t_a392a5da / D3 管理面的**前端入口**）。
 *
 * # 数据来源（三份，全部后端下发，前端不写死任何边 / 状态 / 文案）
 *
 * `GET /business/flow/state-edge`（不传 entity = 9 个实体全量）→ 每个实体：
 * - `base_edges`：代码里的 BASE 边（`EdgeRes`，`configured` 恒 false、`enabled` 恒 true）；
 * - `configs`：库里存活配置行（`ConfigRowRes`）；
 * - `effective_edges`：合并后的有效边（被禁用的边**不出现**在这里）；
 * - `pending_changes`：在跑的配置变更审批单。
 *
 * 三份的叠加规则在 `state-edge-logic.ts::mergeBaseEdges`（纯函数，有单测钉住）。
 *
 * # 写入路径
 *
 * 只有 `PUT /business/flow/state-edge`（改）与 `DELETE`（删），**都只会生成审批单**。
 * 后端 `upsert_state_edge` 先 dry-run 校验再起审批，审批通过的回调才调内核落库。
 * 前端拿到的 `applied = false` + `instance_no` 是「已送审」的成功态。
 */
import { computed, ref, watch } from 'vue';
import { MessagePlugin, type PrimaryTableCol } from 'tdesign-vue-next';
import {
  changeStateEdge,
  deleteStateEdge,
  listStateEdges,
  type EntityStateEdges,
} from '@/api/business/flow/state-edge';
import { ApiError } from '@/api/request';
import {
  STATE_EDGE_PERMISSIONS,
  buildChangeReq,
  changeFormOf,
  changeReasonError,
  changeResultText,
  changeSummary,
  criticalImpactText,
  edgeRowView,
  mergeBaseEdges,
  needsCriticalConfirm,
  pendingChangeText,
  type EdgeChangeForm,
  type EdgeRowView,
} from './state-edge-logic';

/** 抽屉可见（v-model:visible 透传） */
const props = defineProps<{ visible: boolean; initialEntity?: string }>();
/** 双向绑定 */
const emit = defineEmits<{ 'update:visible': [value: boolean] }>();

/** 本地可见态（写回父组件，父组件用 v-model:visible 绑） */
const innerVisible = computed({
  get: () => props.visible,
  set: (value: boolean) => emit('update:visible', value),
});

/** 各实体视图（一次 GET 全量拿到，切换实体不再发请求） */
const entities = ref<EntityStateEdges[]>([]);
/** 当前实体 */
const currentEntity = ref(props.initialEntity ?? '');
/** 加载中 */
const loading = ref(false);
/** 错误（可重试，不白屏） */
const errorMessage = ref('');

/** 实体下拉选项（label 用后端下发的 entity_label） */
const entityOptions = computed(() =>
  entities.value.map((item) => ({ label: `${item.entity_label}（${item.entity}）`, value: item.entity })),
);

/** 当前实体视图 */
const currentView = computed<EntityStateEdges | undefined>(() =>
  entities.value.find((item) => item.entity === currentEntity.value),
);

/** 表格行（BASE 边 ⊙ 配置，文案由纯函数定） */
const rows = computed<EdgeRowView[]>(() => {
  const view = currentView.value;
  if (!view) {
    return [];
  }
  return mergeBaseEdges(view).map((edge) => edgeRowView(view.entity, edge));
});

/** 生效边条数（合并后 `enabled = true` 的） */
const effectiveCount = computed(() => rows.value.filter((row) => row.edge.enabled).length);

/** 在跑审批单 */
const pending = computed(() => currentView.value?.pending_changes ?? []);

/** 表格列 */
const columns: PrimaryTableCol[] = [
  { colKey: 'from', title: '从', width: 140 },
  { colKey: 'to', title: '到', width: 140 },
  { colKey: 'label', title: '文案', width: 120 },
  { colKey: 'mandatory', title: '约束', width: 90 },
  { colKey: 'critical', title: '主链路', width: 80 },
  { colKey: 'reason', title: '原因', width: 120 },
  { colKey: 'auth_code', title: '追加权限码', width: 170 },
  { colKey: 'configured', title: '当前配置', width: 90 },
  { colKey: 'enabled', title: '生效', width: 80 },
  { colKey: 'op', title: '操作', width: 150, fixed: 'right' },
];

/** 拉全量（9 个实体；一次请求就够，切实体不发请求） */
async function load(): Promise<void> {
  loading.value = true;
  errorMessage.value = '';
  try {
    const data = await listStateEdges();
    entities.value = data.list ?? [];
    // 首次打开 / 刷新后当前实体不在列表里 → 落到后端给的第一个（顺序也由后端决定）
    const stillThere = entities.value.some((item) => item.entity === currentEntity.value);
    if (!stillThere) {
      const wanted = props.initialEntity && entities.value.find((item) => item.entity === props.initialEntity);
      currentEntity.value = wanted ? props.initialEntity! : (entities.value[0]?.entity ?? '');
    }
  } catch (error) {
    entities.value = [];
    errorMessage.value = error instanceof ApiError ? error.message : '状态机边配置加载失败';
  } finally {
    loading.value = false;
  }
}

/** 抽屉打开时自动加载（每次打开都拉一次，避免看到别人改完的旧值） */
watch(
  () => props.visible,
  (visible) => {
    if (visible) {
      void load();
    }
  },
  { immediate: true },
);

// ==================== 变更 / 删除 ====================

/** 变更弹窗可见 */
const dialogVisible = ref(false);
/** 弹窗模式：改 / 删（都走审批） */
const dialogMode = ref<'UPSERT' | 'DELETE'>('UPSERT');
/** 当前操作的边 */
const target = ref<EdgeRowView | null>(null);
/** 表单 */
const changeForm = ref<EdgeChangeForm>({ enabled: true, requires_reason: false, label: '', auth_code: '', reason: '' });
/** 提交中 */
const submitting = ref(false);

/**
 * 【E2E-FIX-1】「我确认要禁用主链路必需边」的勾选。
 *
 * 只在「禁用 + 主链路必需边」时出现；没勾就提交 → 前端先挡，后端还会再挡一次（400）。
 * 每次打开弹窗都重置成未勾，避免上一次的确认被顺手复用。
 */
const criticalConfirmed = ref(false);

/** 当前弹窗是不是「禁用主链路边」（决定红字警告 / 强制勾选是否出现） */
const showCriticalWarning = computed(
  () => Boolean(target.value) && needsCriticalConfirm(target.value!.edge, changeForm.value),
);

// 重新打开「启用」时把确认勾选清掉：否则「关→勾→开→再关」能复用上一次的确认，
// 等于让人在不看警告的情况下第二次关掉主链路边。
watch(
  () => changeForm.value.enabled,
  (enabled) => {
    if (enabled) {
      criticalConfirmed.value = false;
    }
  },
);

/** 打开「变更」 */
function openChange(row: EdgeRowView): void {
  target.value = row;
  dialogMode.value = 'UPSERT';
  changeForm.value = changeFormOf(row.edge);
  criticalConfirmed.value = false;
  dialogVisible.value = true;
}

/** 打开「删除配置」（回到 BASE 行为） */
function openDelete(row: EdgeRowView): void {
  target.value = row;
  dialogMode.value = 'DELETE';
  changeForm.value = { enabled: true, requires_reason: row.edge.requires_reason, label: '', auth_code: '', reason: '' };
  criticalConfirmed.value = false;
  dialogVisible.value = true;
}

/** 提交：改 → PUT；删 → DELETE。两者都只生成审批单 */
async function submitChange(): Promise<void> {
  const row = target.value;
  const view = currentView.value;
  if (!row || !view) {
    return;
  }
  const reasonError = changeReasonError(changeForm.value.reason);
  if (reasonError) {
    MessagePlugin.warning(reasonError);
    return;
  }
  // 【E2E-FIX-1】禁用主链路必需边：没勾确认不许提交（后端也有硬门，这里先挡一层，
  // 免得用户填完理由才吃 400）。
  if (dialogMode.value === 'UPSERT' && showCriticalWarning.value && !criticalConfirmed.value) {
    MessagePlugin.warning(`请先勾选「我确认要禁用主链路必需边」（${criticalImpactText(row.edge)}）`);
    return;
  }
  submitting.value = true;
  try {
    const result =
      dialogMode.value === 'DELETE'
        ? await deleteStateEdge({
            entity: view.entity,
            from_status: row.edge.from,
            to_status: row.edge.to,
            // 二次确认：后端缺了直接 400（10001）
            confirm: true,
            reason: changeForm.value.reason.trim(),
          })
        : await changeStateEdge(buildChangeReq(view.entity, row.edge, changeForm.value, criticalConfirmed.value));
    dialogVisible.value = false;
    MessagePlugin.success(changeResultText(result));
    await load();
  } catch (error) {
    // 409（同一状态上已有在跑审批单）/ 400（禁用 mandatory、放宽 requires_reason）/ 403（非超管）都在这
    MessagePlugin.error(error instanceof ApiError ? error.message : '提交审批失败');
  } finally {
    submitting.value = false;
  }
}
</script>

<style scoped lang="less">
.se {
  display: flex;
  flex-direction: column;
  gap: 12px;

  &__tip {
    :deep(.t-alert__message) {
      line-height: 1.7;
    }
  }

  &__bar {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  &__entity {
    width: 260px;
  }

  &__meta {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }

  &__alert {
    margin-bottom: 4px;
  }

  &__empty {
    padding: 24px 0;
    color: var(--td-text-color-placeholder);
    text-align: center;
  }

  &__pending {
    margin-top: 4px;
    padding: 10px 12px;
    background: var(--td-warning-color-light);
    border-radius: 6px;

    &-title {
      margin-bottom: 6px;
      font-size: 13px;
      font-weight: 500;
    }

    ul {
      margin: 0;
      padding-left: 16px;
      font-size: 12px;
      line-height: 1.9;
    }
  }
}

.ce {
  display: flex;
  flex-direction: column;
  gap: 10px;

  &__edge {
    display: flex;
    gap: 10px;
    align-items: center;
    padding: 8px 10px;
    background: var(--td-bg-color-container-hover);
    border-radius: 6px;
  }

  &__label {
    color: var(--td-text-color-secondary);
  }

  &__summary {
    padding: 8px 10px;
    font-size: 13px;
    background: var(--td-brand-color-light);
    border-radius: 6px;
  }

  &__hint {
    margin: 0;
    font-size: 12px;
    line-height: 1.7;
    color: var(--td-text-color-secondary);
  }
}

.form-tip {
  margin-left: 8px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.mono {
  font-family: var(--app-font-mono, monospace);
  font-size: 12px;
}

.muted {
  color: var(--td-text-color-placeholder);
}
</style>
