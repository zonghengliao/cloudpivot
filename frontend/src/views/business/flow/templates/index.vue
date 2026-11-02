<template>
  <div class="flow-tpl-page" data-testid="flow-tpl-page">
    <!-- 头部：标题 + 计数 + 刷新 + 分类 Tab -->
    <t-card :bordered="false" class="head-card">
      <div class="head">
        <div class="head__text">
          <h3 class="head__title">流程模板库</h3>
          <p class="head__desc">
            预置标准流程开箱即用：看流程图、另存为改成自己的、随时启用停用（在跑的审批单不受影响）。
          </p>
        </div>
        <t-space align="center">
          <span class="head__count" data-testid="tpl-total">共 {{ total }} 个模板</span>
          <t-button variant="outline" size="small" :loading="loading" @click="loadTemplates">
            <template #icon><t-icon name="refresh" /></template>
            刷新
          </t-button>
        </t-space>
      </div>
      <t-tabs v-model="activeCategory" class="head__tabs">
        <t-tab-panel value="__all__" label="全部" />
        <t-tab-panel
          v-for="group in groups"
          :key="group.category"
          :value="group.category"
          :label="`${group.category}（${group.items.length}）`"
        />
      </t-tabs>
    </t-card>

    <!-- 错误态：不静默失败，给重试 -->
    <t-alert v-if="errorMessage" class="page-alert" theme="error" :message="errorMessage">
      <template #operation>
        <t-button size="small" variant="outline" @click="loadTemplates">重试</t-button>
      </template>
    </t-alert>

    <t-loading :loading="loading" show-overlay>
      <div v-if="visibleGroups.length === 0 && !loading" class="empty" data-testid="tpl-empty">
        <t-icon name="template" size="32px" />
        <p>{{ activeCategory === '__all__' ? '还没有任何流程模板' : '该分类下没有模板' }}</p>
      </div>

      <section
        v-for="group in visibleGroups"
        :key="group.category"
        class="group"
        :data-testid="`tpl-group-${group.category}`"
      >
        <h4 class="group__title">
          {{ group.category }}
          <span class="group__count">{{ group.items.length }}</span>
        </h4>
        <t-row :gutter="[16, 16]">
          <t-col v-for="item in group.items" :key="item.id" :xs="12" :md="6" :xl="4">
            <t-card
              :bordered="false"
              class="tpl-card"
              :class="{ 'tpl-card--system': item.is_system }"
              :data-testid="`tpl-card-${item.flow_code}`"
            >
              <!-- 标题行：名称 + 标准/默认标签 + 启用状态 -->
              <div class="tpl-card__head">
                <span class="tpl-card__name" :title="item.flow_name" data-testid="tpl-name">
                  {{ item.flow_name }}
                </span>
                <t-space size="4px" :break-line="false">
                  <t-tag v-if="item.is_system" size="small" theme="primary" variant="light" data-testid="tpl-system-tag">
                    标准
                  </t-tag>
                  <t-tag v-if="item.is_default" size="small" variant="light" data-testid="tpl-default-tag">
                    默认
                  </t-tag>
                  <t-tag
                    size="small"
                    :theme="statusMeta(item.status).theme"
                    variant="light"
                    data-testid="tpl-status"
                  >
                    {{ statusMeta(item.status).label }}
                  </t-tag>
                </t-space>
              </div>

              <!-- 适用场景（条件分流标签 / 说明首句） -->
              <p class="tpl-card__summary" data-testid="tpl-summary">
                {{ item.summary || '（该模板还没有配置条件分流）' }}
              </p>

              <!-- 事实行：节点 / 连线 / 已用次数 / 最近使用 -->
              <div class="tpl-card__meta" data-testid="tpl-meta">
                <span>{{ item.node_count }} 节点 · {{ item.edge_count }} 连线</span>
                <span class="tpl-card__dot">·</span>
                <span>已用 {{ item.usage_count }} 次</span>
                <template v-if="item.last_used_at">
                  <span class="tpl-card__dot">·</span>
                  <span>最近 {{ formatDate(item.last_used_at) }}</span>
                </template>
              </div>

              <div class="tpl-card__code" data-testid="tpl-code">
                <code>{{ item.flow_code }}</code>
                <span class="tpl-card__biz">{{ item.biz_type }}</span>
              </div>

              <!-- 操作 -->
              <div class="tpl-card__actions">
                <t-button
                  size="small"
                  variant="text"
                  :disabled="!item.view_version_id"
                  data-testid="tpl-view"
                  @click="openGraph(item)"
                >
                  查看流程图
                </t-button>
                <t-button
                  v-privilege="[PERM.flowCreate]"
                  size="small"
                  variant="text"
                  data-testid="tpl-duplicate"
                  @click="openDuplicate(item)"
                >
                  另存为
                </t-button>
                <t-button
                  v-if="item.status === 'PUBLISHED'"
                  v-privilege="[PERM.flowPublish]"
                  size="small"
                  variant="text"
                  theme="danger"
                  data-testid="tpl-disable"
                  @click="askDisable(item)"
                >
                  停用
                </t-button>
                <t-button
                  v-else
                  v-privilege="[PERM.flowPublish]"
                  size="small"
                  variant="text"
                  theme="primary"
                  data-testid="tpl-enable"
                  @click="askEnable(item)"
                >
                  启用
                </t-button>
              </div>
            </t-card>
          </t-col>
        </t-row>
      </section>
    </t-loading>

    <!-- 只读流程图抽屉（复用 FlowCanvas 的只读包装 FlowPreview，不重画） -->
    <t-drawer
      v-model:visible="graphVisible"
      :header="graphTitle"
      size="900px"
      :close-btn="true"
      close-on-overlay-click
      class="graph-drawer"
    >
      <div v-if="graphLoading" class="graph-state">加载流程图中…</div>
      <t-alert v-else-if="graphError" theme="error" :message="graphError">
        <template #operation>
          <t-button size="small" variant="outline" @click="reloadGraph">重试</t-button>
        </template>
      </t-alert>
      <template v-else-if="graphVersion">
        <div class="graph-meta" data-testid="graph-meta">
          v{{ graphVersion.version_no }} ·
          {{ graphVersion.nodes.length }} 节点 / {{ graphVersion.edges.length }} 连线 ·
          {{ graphVersion.editable ? '草稿（可编辑）' : '已发布（只读）' }}
        </div>
        <FlowPreview
          :nodes="graphVersion.nodes"
          :edges="graphVersion.edges"
          height="calc(100vh - 220px)"
        />
      </template>
      <div v-else class="graph-state">该模板还没有可查看的版本。</div>
      <!-- 显式关闭：TDesign 的 drawer 头在这里不渲染关闭图标（header 传的是纯文本），
           只靠点遮罩关闭对用户不友好，所以底部给一个明确的「关闭」。 -->
      <template #footer>
        <t-button variant="outline" data-testid="graph-close" @click="graphVisible = false">
          关闭
        </t-button>
      </template>
    </t-drawer>

    <!-- 另存为 -->
    <t-dialog
      v-model:visible="duplicateVisible"
      header="另存为自定义流程"
      :confirm-btn="{ content: '复制并去编辑', loading: duplicateLoading, theme: 'primary' }"
      :cancel-btn="{ content: '取消' }"
      width="520px"
      @confirm="submitDuplicate"
    >
      <t-alert theme="info" :message="`将从「${sourceName}」复制一份流程定义（节点 + 连线 + 审批人），原模板不动。`" />
      <t-form ref="duplicateFormRef" :data="duplicateForm" :rules="duplicateRules" label-align="top" class="dup-form">
        <t-form-item label="新流程编码" name="new_flow_code">
          <t-input
            v-model="duplicateForm.new_flow_code"
            placeholder="小写字母开头，可含数字与下划线，如 po_approve_custom"
            data-testid="dup-code"
          />
        </t-form-item>
        <t-form-item label="新流程名称" name="new_flow_name">
          <t-input v-model="duplicateForm.new_flow_name" placeholder="如：我们的采购审批" data-testid="dup-name" />
        </t-form-item>
        <p class="dup-hint">副本状态是草稿（不抢默认流程），改完在流程定义页发布才生效。</p>
      </t-form>
    </t-dialog>

    <!-- 停用 / 启用 二次确认 -->
    <t-dialog
      v-model:visible="toggleVisible"
      :header="toggleKind === 'disable' ? '停用流程' : '启用流程'"
      :theme="toggleKind === 'disable' ? 'warning' : 'info'"
      :confirm-btn="{
        content: toggleKind === 'disable' ? '确认停用' : '确认启用',
        loading: toggleLoading,
        theme: toggleKind === 'disable' ? 'danger' : 'primary',
      }"
      :cancel-btn="{ content: '取消' }"
      width="520px"
      @confirm="submitToggle"
    >
      <template v-if="toggleItem">
        <t-alert
          v-if="toggleKind === 'disable' && toggleItem.is_default"
          theme="warning"
          data-testid="toggle-default-warning"
          message="这是该业务的默认流程：停用后，该业务的单据提交将直接生效、不走审批（历史审批单仍按原版本跑完）。"
        />
        <p class="toggle-text" data-testid="toggle-text">
          {{ toggleMessage }}
        </p>
      </template>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
/**
 * 流程模板库（`/flow/templates`，卡面 P-FLOW-5）。
 *
 * # 这一页解决的问题
 *
 * 「系统里到底有哪些标准流程？」——以前只能去流程定义列表里翻，且看不到「谁用过几次」。
 * 这一页把标准模板按业务域摊开：一眼看到适用场景（条件分流阈值）、节点规模、已用次数、
 * 启用状态，并能 **看流程图 / 另存为改成自己的 / 启用停用**。
 *
 * # 三条数据口径（全部来自后端，前端不算也不猜）
 *
 * 1. 分组 = 后端 `category`（`GET /business/flow/def/templates` 已分好组）；
 * 2. 「标准」标签 = `is_system`（系统预置），另存为出来的副本恒 false —— 客户的流程不冒充标准；
 * 3. 流程图 = `view_version_id` 指向的版本（发布版优先，只有草稿的副本退到最新草稿），
 *    渲染一律走 `FlowPreview`（它内部就是 `FlowCanvas` 的 readonly 模式，**不重画一套**）。
 *
 * # 为什么「停用」要专门警告默认流程（O5）
 *
 * 停用默认流程不是「少一个选项」，而是**该业务从此不走审批**（`default_def_of_biz_type`
 * 只认 `status = 'PUBLISHED'`）。这一句必须写在确认框里，不能等用户提交单据时才发现。
 */
import { computed, onMounted, ref } from 'vue';
import { useRouter } from 'vue-router';
import { MessagePlugin, type FormRules, type FormInstanceFunctions } from 'tdesign-vue-next';
import {
  duplicateFlowDef,
  enableFlowDef,
  getFlowTemplates,
  getFlowVersion,
  offlineFlowDef,
  publishFlowDef,
  type FlowDefStatus,
  type FlowTemplateGroup,
  type FlowTemplateItem,
  type FlowVersion,
} from '@/api/business/flow-api';
import { ApiError } from '@/api/request';
import { PERM } from '@/constants/permissions';
import FlowPreview from '@/views/business/flow/components/FlowPreview.vue';

const router = useRouter();

/** 模板库数据（后端已分组）。 */
const groups = ref<FlowTemplateGroup[]>([]);
/** 模板总数。 */
const total = ref(0);
/** 首次加载 / 刷新中。 */
const loading = ref(false);
/** 加载失败文案（失败就显示错误态，不假装空列表）。 */
const errorMessage = ref('');
/** 当前分类 Tab（`__all__` = 全部）。 */
const activeCategory = ref('__all__');

// ---------- 只读流程图 ----------
const graphVisible = ref(false);
const graphLoading = ref(false);
const graphError = ref('');
const graphVersion = ref<FlowVersion | null>(null);
const graphItem = ref<FlowTemplateItem | null>(null);

// ---------- 另存为 ----------
const duplicateVisible = ref(false);
const duplicateLoading = ref(false);
const sourceItem = ref<FlowTemplateItem | null>(null);
const duplicateFormRef = ref<FormInstanceFunctions>();
const duplicateForm = ref({ new_flow_code: '', new_flow_name: '' });

// ---------- 启用 / 停用 ----------
const toggleVisible = ref(false);
const toggleLoading = ref(false);
const toggleKind = ref<'enable' | 'disable'>('disable');
const toggleItem = ref<FlowTemplateItem | null>(null);

/** 状态文案 / 配色（模板库用「启用中 / 未启用」，与流程定义页的「已发布」区分）。 */
const STATUS_META: Record<string, { label: string; theme: 'success' | 'warning' | 'default' }> = {
  PUBLISHED: { label: '启用中', theme: 'success' },
  DRAFT: { label: '未启用（草稿）', theme: 'default' },
  DISABLED: { label: '已停用', theme: 'warning' },
};

function statusMeta(status: FlowDefStatus | string) {
  return STATUS_META[status] ?? { label: status, theme: 'default' as const };
}

/** 当前 Tab 下要渲染的分组。 */
const visibleGroups = computed(() =>
  activeCategory.value === '__all__'
    ? groups.value
    : groups.value.filter((group) => group.category === activeCategory.value),
);

const graphTitle = computed(() =>
  graphItem.value ? `${graphItem.value.flow_name} · 流程图` : '流程图',
);

const sourceName = computed(() => sourceItem.value?.flow_name ?? '');

const toggleMessage = computed(() => {
  const item = toggleItem.value;
  if (!item) {
    return '';
  }
  if (toggleKind.value === 'disable') {
    return `确认停用「${item.flow_name}」？停用后不能再发起新的审批单，已在跑的审批单不受影响。`;
  }
  if (item.status === 'DISABLED') {
    return `确认启用「${item.flow_name}」？启用后该业务恢复走审批，用的是停用前那个已发布版本（不重新发布）。`;
  }
  return `确认启用「${item.flow_name}」？启用以发布当前草稿为准，图不完整会被后端拒绝并给出原因。`;
});

/** 编码规则（与后端 `ck_biz_flow_def_code` / `normalize_flow_code` 一致）。 */
const duplicateRules: FormRules = {
  new_flow_code: [
    { required: true, message: '必须填新流程编码', type: 'error' },
    {
      pattern: /^[a-z][a-z0-9_]{1,63}$/,
      message: '小写字母开头，只能包含小写字母 / 数字 / 下划线，长度 2–64',
      type: 'error',
    },
  ],
  new_flow_name: [
    { required: true, message: '必须填新流程名称', type: 'error' },
    { min: 2, max: 100, message: '名称长度 2–100', type: 'error' },
  ],
};

function formatDate(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return value;
  }
  const pad = (num: number) => String(num).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;
}

async function loadTemplates() {
  loading.value = true;
  errorMessage.value = '';
  try {
    const res = await getFlowTemplates();
    groups.value = res.groups;
    total.value = res.total;
    // 当前 Tab 被删空（分类改名）时退回「全部」，避免停在空白页
    if (
      activeCategory.value !== '__all__' &&
      !res.groups.some((group) => group.category === activeCategory.value)
    ) {
      activeCategory.value = '__all__';
    }
  } catch (error) {
    groups.value = [];
    total.value = 0;
    errorMessage.value = error instanceof ApiError ? error.message : '模板库加载失败，请重试';
  } finally {
    loading.value = false;
  }
}

// ---------- 查看流程图 ----------

async function loadGraph(item: FlowTemplateItem) {
  if (!item.view_version_id) {
    graphVersion.value = null;
    return;
  }
  graphLoading.value = true;
  graphError.value = '';
  try {
    graphVersion.value = await getFlowVersion(item.view_version_id);
  } catch (error) {
    graphVersion.value = null;
    graphError.value = error instanceof ApiError ? error.message : '流程图加载失败';
  } finally {
    graphLoading.value = false;
  }
}

function openGraph(item: FlowTemplateItem) {
  graphItem.value = item;
  graphVersion.value = null;
  graphError.value = '';
  graphVisible.value = true;
  void loadGraph(item);
}

function reloadGraph() {
  if (graphItem.value) {
    void loadGraph(graphItem.value);
  }
}

// ---------- 另存为 ----------

function openDuplicate(item: FlowTemplateItem) {
  sourceItem.value = item;
  duplicateForm.value = {
    new_flow_code: `${item.flow_code}_custom`,
    new_flow_name: `${item.flow_name.replace(/（.*?）/g, '')}（自定义）`,
  };
  duplicateVisible.value = true;
}

async function submitDuplicate() {
  const item = sourceItem.value;
  if (!item) {
    return;
  }
  const valid = await duplicateFormRef.value?.validate();
  if (valid !== true) {
    return;
  }
  duplicateLoading.value = true;
  try {
    const res = await duplicateFlowDef(item.id, {
      new_flow_code: duplicateForm.value.new_flow_code.trim(),
      new_flow_name: duplicateForm.value.new_flow_name.trim(),
    });
    duplicateVisible.value = false;
    MessagePlugin.success(
      `已复制为「${res.flow_name}」（草稿 v${res.version_no}，${res.node_count} 节点）`,
    );
    await loadTemplates();
    // 直接进编辑器：用户改完自己发布（另存为的语义就是「复制一份可改」）
    void router.push({
      path: `/flow/def/${res.id}/edit`,
      query: { vid: res.version_id },
    });
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '另存为失败');
  } finally {
    duplicateLoading.value = false;
  }
}

// ---------- 启用 / 停用 ----------

function askDisable(item: FlowTemplateItem) {
  toggleItem.value = item;
  toggleKind.value = 'disable';
  toggleVisible.value = true;
}

function askEnable(item: FlowTemplateItem) {
  toggleItem.value = item;
  toggleKind.value = 'enable';
  toggleVisible.value = true;
}

async function submitToggle() {
  const item = toggleItem.value;
  if (!item) {
    return;
  }
  toggleLoading.value = true;
  try {
    if (toggleKind.value === 'disable') {
      await offlineFlowDef(item.id);
      MessagePlugin.success(`已停用「${item.flow_name}」（在跑审批单不受影响）`);
    } else if (item.status === 'DISABLED') {
      // 停用过的流程：只把状态位拨回 PUBLISHED（不产生新版本、不再跑发布校验）
      await enableFlowDef(item.id);
      MessagePlugin.success(`已启用「${item.flow_name}」，该业务恢复走审批`);
    } else {
      // 草稿：走发布（图不完整会被后端拒绝并给出原因）
      await publishFlowDef(item.id);
      MessagePlugin.success(`已启用「${item.flow_name}」，该业务恢复走审批`);
    }
    toggleVisible.value = false;
    await loadTemplates();
  } catch (error) {
    MessagePlugin.error(
      error instanceof ApiError
        ? error.message
        : toggleKind.value === 'disable'
          ? '停用失败'
          : '启用失败（草稿图不完整会被拒绝，请去流程编辑器修正）',
    );
  } finally {
    toggleLoading.value = false;
  }
}

onMounted(loadTemplates);
</script>

<style scoped lang="less">
.flow-tpl-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.head-card {
  border-radius: 8px;
}

.head {
  display: flex;
  align-items: flex-start;
  justify-content: space-between;
  gap: 16px;
}

.head__title {
  margin: 0;
  font-size: 18px;
  font-weight: 600;
}

.head__desc {
  margin: 6px 0 0;
  color: var(--td-text-color-secondary);
  font-size: 13px;
}

.head__count {
  color: var(--td-text-color-secondary);
  font-size: 13px;
  white-space: nowrap;
}

.head__tabs {
  margin-top: 12px;
}

.page-alert {
  border-radius: 8px;
}

.empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 8px;
  padding: 48px 0;
  color: var(--td-text-color-secondary);
}

.group {
  margin-bottom: 20px;
}

.group__title {
  margin: 4px 0 12px;
  font-size: 15px;
  font-weight: 600;
  display: flex;
  align-items: center;
  gap: 8px;
}

.group__count {
  padding: 0 8px;
  border-radius: 10px;
  background: var(--td-bg-color-container-hover);
  color: var(--td-text-color-secondary);
  font-size: 12px;
  font-weight: 400;
  line-height: 18px;
}

.tpl-card {
  height: 100%;
  border-radius: 8px;
  display: flex;
  flex-direction: column;

  &--system {
    border-left: 3px solid var(--td-brand-color);
  }
}

.tpl-card__head {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
}

.tpl-card__name {
  font-size: 14px;
  font-weight: 600;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.tpl-card__summary {
  margin: 10px 0 8px;
  min-height: 38px;
  color: var(--td-text-color-primary);
  font-size: 13px;
  line-height: 19px;
  display: -webkit-box;
  -webkit-line-clamp: 2;
  -webkit-box-orient: vertical;
  overflow: hidden;
}

.tpl-card__meta {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 4px;
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.tpl-card__dot {
  color: var(--td-text-color-placeholder);
}

.tpl-card__code {
  margin-top: 8px;
  display: flex;
  align-items: center;
  gap: 8px;
  font-size: 12px;
  color: var(--td-text-color-secondary);

  code {
    padding: 1px 6px;
    border-radius: 4px;
    background: var(--td-bg-color-container-hover);
  }
}

.tpl-card__biz {
  color: var(--td-text-color-placeholder);
}

.tpl-card__actions {
  margin-top: 10px;
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 4px;
  border-top: 1px solid var(--td-component-stroke);
  padding-top: 8px;
}

.graph-state {
  padding: 24px 0;
  color: var(--td-text-color-secondary);
  text-align: center;
}

.graph-meta {
  margin-bottom: 10px;
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.dup-form {
  margin-top: 12px;
}

.dup-hint {
  margin: 0;
  color: var(--td-text-color-secondary);
  font-size: 12px;
}

.toggle-text {
  margin: 10px 0 0;
  font-size: 13px;
  line-height: 20px;
}
</style>
