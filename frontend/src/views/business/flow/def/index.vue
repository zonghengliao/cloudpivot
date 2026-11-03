<template>
  <div class="flow-def-page">
    <!-- 筛选区 -->
    <t-card :bordered="false" class="filter-card">
      <t-row :gutter="[16, 12]">
        <t-col :span="5">
          <t-input v-model="query.keyword" placeholder="流程编码 / 名称" clearable @enter="handleSearch">
            <template #prefix-icon><t-icon name="search" /></template>
          </t-input>
        </t-col>
        <t-col :span="3">
          <t-select v-model="query.status" placeholder="状态" clearable :options="statusOpts" />
        </t-col>
        <t-col :span="3">
          <t-input v-model="query.biz_type" placeholder="业务类型，如 TASK_ACCEPT" clearable @enter="handleSearch" />
        </t-col>
        <t-col :span="13">
          <t-space>
            <t-button theme="primary" @click="handleSearch">
              <template #icon><t-icon name="search" /></template>
              查询
            </t-button>
            <t-button variant="outline" @click="handleReset">重置</t-button>
          </t-space>
        </t-col>
      </t-row>
    </t-card>

    <!-- 工具条 -->
    <t-card :bordered="false" class="toolbar-card">
      <div class="toolbar">
        <t-space align="center">
          <t-button v-privilege="[PERM.flowCreate]" theme="primary" size="small" @click="openCreate">
            <template #icon><t-icon name="add" /></template>
            新建流程
          </t-button>
          <!-- 状态机边配置入口（D3 管理面前端入口，卡 t_a392a5da）：读 BASE 边 + 配置，改配置走审批 -->
          <t-button
            v-privilege="[PERM.flowStateEdgeList]"
            variant="outline"
            size="small"
            data-testid="btn-state-edge"
            @click="openStateEdge()"
          >
            <template #icon><t-icon name="setting" /></template>
            边配置
          </t-button>
          <t-button variant="outline" size="small" :loading="loading" @click="loadList">
            <template #icon><t-icon name="refresh" /></template>
            刷新
          </t-button>
        </t-space>
        <span class="toolbar__count">共 {{ total }} 个流程 · 状态取值 DRAFT / PUBLISHED / DISABLED</span>
      </div>
    </t-card>

    <!-- 列表 -->
    <t-card :bordered="false" class="table-card">
      <t-alert v-if="errorMessage" class="table-alert" theme="error" :message="errorMessage">
        <template #operation>
          <t-button size="small" variant="outline" @click="loadList">重试</t-button>
        </template>
      </t-alert>

      <t-table
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="tablePagination"
        :hover="true"
        stripe
        @page-change="handlePageChange"
      >
        <template #flow_code="{ row }">
          <span class="cell-code">{{ row.flow_code }}</span>
        </template>

        <template #flow_name="{ row }">
          <div class="cell-name">
            <t-link theme="primary" @click="openEditor(row)">{{ row.flow_name }}</t-link>
            <t-tag v-if="row.is_default" size="small" variant="light" theme="primary">默认</t-tag>
          </div>
        </template>

        <template #status="{ row }">
          <StatusBadge domain="flow-def" :status="row.status" />
        </template>

        <template #version="{ row }">
          <span v-if="row.current_version_no > 0">v{{ row.current_version_no }}</span>
          <span v-else class="cell-muted">未发布</span>
        </template>

        <template #graph_size="{ row }">
          <span class="cell-muted">{{ row.node_count }} 节点 / {{ row.edge_count }} 连线</span>
        </template>

        <template #updated_at="{ row }">
          {{ formatTime(row.updated_at) }}
        </template>

        <template #op="{ row }">
          <t-space size="small">
            <t-link v-privilege="[PERM.flowUpdate]" theme="primary" @click="openEditor(row)">编辑</t-link>
            <t-link v-privilege="[PERM.flowPublish]" theme="primary" :disabled="row.status === 'PUBLISHED'" @click="openPublish(row)">
              发布
            </t-link>
            <t-link
              v-privilege="[PERM.flowPublish]"
              theme="warning"
              :disabled="row.status !== 'PUBLISHED'"
              @click="openOffline(row)"
            >
              停用
            </t-link>
            <t-link theme="default" @click="openVersions(row)">版本历史</t-link>
            <t-link v-privilege="[PERM.flowDelete]" theme="danger" @click="openDelete(row)">删除</t-link>
          </t-space>
        </template>

        <template #empty>
          <div class="table-empty">
            <t-icon name="file-copy" size="24px" />
            <p>还没有流程定义。点「新建流程」建一张最简图（开始 → 结束），再进编辑器扩展。</p>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- 新建流程 -->
    <t-dialog
      v-model:visible="createVisible"
      header="新建流程"
      width="560px"
      :confirm-btn="{ content: '创建并进入编辑器', loading: creating }"
      @confirm="submitCreate"
    >
      <t-form :data="createForm" :rules="createRules" label-align="top">
        <t-form-item label="流程编码" name="flow_code">
          <t-input v-model="createForm.flow_code" placeholder="小写字母开头，如 task_accept（创建后不可改）" :maxlength="64" />
        </t-form-item>
        <t-form-item label="流程名称" name="flow_name">
          <t-input v-model="createForm.flow_name" placeholder="如 任务验收" :maxlength="100" />
        </t-form-item>
        <t-form-item label="业务类型" name="biz_type">
          <t-input v-model="createForm.biz_type" placeholder="如 TASK_ACCEPT" :maxlength="64" />
        </t-form-item>
        <t-form-item label="分类">
          <t-input v-model="createForm.category" placeholder="可选，如 人事 / 财务" :maxlength="64" />
        </t-form-item>
        <t-form-item label="说明">
          <t-textarea v-model="createForm.description" placeholder="可选" :autosize="{ minRows: 2, maxRows: 5 }" :maxlength="500" />
        </t-form-item>
        <t-form-item label="默认流程">
          <t-switch v-model="createForm.is_default" />
          <span class="form-tip">同一业务类型只有一个默认流程</span>
        </t-form-item>
        <t-form-item label="允许超时自动通过">
          <t-switch v-model="createForm.allow_auto_approve" />
          <span class="form-tip">流程级总闸；关掉后节点上的「自动通过」不生效</span>
        </t-form-item>
      </t-form>
    </t-dialog>

    <!-- 版本历史 -->
    <t-drawer v-model:visible="versionsVisible" size="560px" header="版本历史" :footer="false">
      <div v-if="versionLoading" class="drawer-loading">加载中…</div>
      <t-table v-else row-key="id" :data="versions" :columns="versionColumns" size="small" :hover="true">
        <template #version_no="{ row }">
          <span>v{{ row.version_no }}</span>
          <t-tag v-if="row.is_current" size="small" theme="success" variant="light" class="version-current">当前</t-tag>
        </template>
        <template #version_status="{ row }">
          <t-tag size="small" variant="light" :theme="versionTheme(row.version_status)">
            {{ flowVersionStatusMeta(row.version_status).label }}
          </t-tag>
        </template>
        <template #published_at="{ row }">
          {{ row.published_at ? formatTime(row.published_at) : '—' }}
        </template>
        <template #op="{ row }">
          <t-space size="small">
            <t-link theme="primary" @click="openVersionEditor(row)">查看 / 编辑</t-link>
          </t-space>
        </template>
        <template #empty>
          <div class="table-empty"><p>暂无版本</p></div>
        </template>
      </t-table>
    </t-drawer>

    <!-- 发布前二次确认（校验由后端 publish 跑 R1–R15） -->
    <t-dialog
      v-model:visible="publishVisible"
      theme="warning"
      header="发布流程"
      width="480px"
      :confirm-btn="{ content: '发布', theme: 'warning', loading: publishing }"
      @confirm="submitPublish"
    >
      <p>发布 <strong>{{ currentRow?.flow_name }}</strong>？</p>
      <p class="dialog-tip">
        发布会跑完整校验（恰好一个开始节点、至少一个结束节点、无环、审批人已配置…）。有任意一条不通过会直接失败并给出原因，图不会进入已发布状态。
      </p>
    </t-dialog>

    <!-- 停用确认 -->
    <t-dialog
      v-model:visible="offlineVisible"
      theme="warning"
      header="停用流程"
      width="480px"
      :confirm-btn="{ content: '停用', theme: 'warning', loading: offlining }"
      @confirm="submitOffline"
    >
      <p>停用 <strong>{{ currentRow?.flow_name }}</strong>？</p>
      <p class="dialog-tip">停用后不能再发起新审批单；**已经在跑的单子不受影响**，会继续按原版本走完。</p>
    </t-dialog>

    <!-- 删除确认 -->
    <t-dialog
      v-model:visible="deleteVisible"
      theme="danger"
      header="删除流程"
      width="480px"
      :confirm-btn="{ content: '删除', theme: 'danger', loading: deleting }"
      @confirm="submitDelete"
    >
      <p>删除 <strong>{{ currentRow?.flow_name }}</strong>（{{ currentRow?.flow_code }}）？</p>
      <p class="dialog-tip">软删；有进行中的审批单时后端会拒绝（409）。编码在软删后可被新流程复用。</p>
    </t-dialog>

    <!-- 状态机边配置（D3 管理面的前端入口；改配置一律提交审批，不直写库） -->
    <StateEdgePanel v-model:visible="stateEdgeVisible" :initial-entity="stateEdgeEntity" />
  </div>
</template>

<script setup lang="ts">
/**
 * 流程定义列表（卡面页面 1 / `/flow/def`）。
 *
 * # 数据来源
 *
 * 全部走 D1 已交付的接口（`flow-api.ts`），没有 mock：
 * 分页 / 发布 / 停用 / 删除 / 版本列表都是真实请求，失败按后端 `msg` 提示。
 *
 * # 状态取值
 *
 * `DRAFT / PUBLISHED / DISABLED`（**停用叫 DISABLED**，不是 OFFLINE ——
 * 与后端 `dto::DEF_STATUSES` 逐字一致）。徽标文案集中在 `constants/status-map.ts`。
 *
 * # 编辑器入口
 *
 * 「编辑」与「版本历史 → 查看 / 编辑」都进 `/flow/def/{defId}/edit`，
 * 区别只是后者带 `?vid=` 指定要看哪一版（编辑器据此决定加载哪张图 / 是否只读）。
 */
import { computed, onMounted, ref } from 'vue';
import { useRoute, useRouter } from 'vue-router';
import { MessagePlugin } from 'tdesign-vue-next';
import type { PrimaryTableCol, FormRules } from 'tdesign-vue-next';
import {
  createFlowDef,
  deleteFlowDef,
  listFlowVersions,
  offlineFlowDef,
  pageFlowDefs,
  publishFlowDef,
  type FlowDef,
  type FlowVersionSummary,
} from '@/api/business/flow-api';
import { ApiError } from '@/api/request';
import { PERM } from '@/constants/permissions';
import { flowVersionStatusMeta } from '@/constants/status-map';
import StatusBadge from '@/views/business/task/components/StatusBadge.vue';
import StateEdgePanel from './StateEdgePanel.vue';

const router = useRouter();
const route = useRoute();

/** 查询条件 */
const query = ref<{ keyword: string; status: string; biz_type: string }>({ keyword: '', status: '', biz_type: '' });

/** 状态筛选选项（顺序 = 状态机推进顺序） */
const statusOpts = [
  { label: '草稿', value: 'DRAFT' },
  { label: '已发布', value: 'PUBLISHED' },
  { label: '已停用', value: 'DISABLED' },
];

/** 列表数据 */
const rows = ref<FlowDef[]>([]);
/** 总条数 */
const total = ref(0);
/** 加载中 */
const loading = ref(false);
/** 错误提示（可重试，不白屏） */
const errorMessage = ref('');
/** 分页 */
const pagination = ref({ current: 1, pageSize: 20 });

/** 表格分页配置 */
const tablePagination = computed(() => ({
  current: pagination.value.current,
  pageSize: pagination.value.pageSize,
  total: total.value,
  showJumper: true,
  pageSizeOptions: [10, 20, 50],
}));

/** 表格列 */
const columns: PrimaryTableCol[] = [
  { colKey: 'flow_code', title: '编码', width: 180 },
  { colKey: 'flow_name', title: '名称', minWidth: 180 },
  { colKey: 'status', title: '状态', width: 100 },
  { colKey: 'version', title: '当前版本', width: 110 },
  { colKey: 'graph_size', title: '图规模', width: 150 },
  { colKey: 'biz_type', title: '业务类型', width: 140 },
  { colKey: 'updated_at', title: '更新时间', width: 170 },
  { colKey: 'op', title: '操作', width: 300, fixed: 'right' },
];

/** 加载列表 */
async function loadList(): Promise<void> {
  loading.value = true;
  errorMessage.value = '';
  try {
    const data = await pageFlowDefs({
      page: pagination.value.current,
      page_size: pagination.value.pageSize,
      keyword: query.value.keyword,
      status: query.value.status,
      biz_type: query.value.biz_type,
    });
    rows.value = data.list ?? [];
    total.value = data.total ?? 0;
  } catch (error) {
    rows.value = [];
    total.value = 0;
    errorMessage.value = error instanceof ApiError ? error.message : '流程定义加载失败';
  } finally {
    loading.value = false;
  }
}

/** 查询（回到第一页） */
function handleSearch(): void {
  pagination.value.current = 1;
  void loadList();
}

/** 重置筛选 */
function handleReset(): void {
  query.value = { keyword: '', status: '', biz_type: '' };
  handleSearch();
}

/** 翻页 */
function handlePageChange(pageInfo: { current: number; pageSize: number }): void {
  pagination.value.current = pageInfo.current;
  pagination.value.pageSize = pageInfo.pageSize;
  void loadList();
}

/** 时间格式化（后端下发 RFC3339，按本地时区展示到分钟） */
function formatTime(value: string | null): string {
  if (!value) {
    return '—';
  }
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return value;
  }
  const pad = (input: number): string => String(input).padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}

/** 版本状态 → t-tag 主题 */
function versionTheme(status: string): 'default' | 'primary' | 'success' | 'warning' | 'danger' {
  return flowVersionStatusMeta(status).theme;
}

// ==================== 新建 ====================

/** 新建弹窗可见 */
const createVisible = ref(false);
/** 创建中 */
const creating = ref(false);
/** 新建表单（本地形状全是字符串，提交时再转成后端的「可空字段」口径） */
const createForm = ref({
  flow_code: '',
  flow_name: '',
  biz_type: '',
  category: '',
  description: '',
  is_default: false,
  allow_auto_approve: false,
});

/** 表单校验规则（与后端 `normalize_flow_code` 的口径一致） */
const createRules: FormRules = {
  flow_code: [
    { required: true, message: '请填写流程编码' },
    {
      pattern: /^[a-z][a-z0-9_]{1,63}$/,
      message: '小写字母开头，只能包含小写字母、数字、下划线，至少 2 个字符',
    },
  ],
  flow_name: [{ required: true, message: '请填写流程名称' }],
  biz_type: [{ required: true, message: '请填写业务类型' }],
};

/** 打开新建弹窗 */
function openCreate(): void {
  createForm.value = {
    flow_code: '',
    flow_name: '',
    biz_type: '',
    category: '',
    description: '',
    is_default: false,
    allow_auto_approve: false,
  };
  createVisible.value = true;
}

/** 提交新建：成功后直接进编辑器（后端已经建好 v1 草稿 + 开始/结束节点） */
async function submitCreate(): Promise<void> {
  if (!createForm.value.flow_code.trim() || !createForm.value.flow_name.trim() || !createForm.value.biz_type.trim()) {
    MessagePlugin.warning('编码 / 名称 / 业务类型都要填');
    return;
  }
  creating.value = true;
  try {
    const result = await createFlowDef({
      flow_code: createForm.value.flow_code.trim(),
      flow_name: createForm.value.flow_name.trim(),
      biz_type: createForm.value.biz_type.trim(),
      category: createForm.value.category.trim() || null,
      description: createForm.value.description.trim() || null,
      is_default: createForm.value.is_default,
      allow_auto_approve: createForm.value.allow_auto_approve,
    });
    createVisible.value = false;
    MessagePlugin.success(`流程已创建（v${result.version_no} 草稿，${result.node_count} 个节点）`);
    await loadList();
    void router.push({ path: `/flow/def/${result.id}/edit`, query: { vid: result.version_id } });
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '创建失败');
  } finally {
    creating.value = false;
  }
}

// ==================== 编辑 ====================

/** 进编辑器（默认打开最新草稿 / 当前发布版） */
function openEditor(row: FlowDef): void {
  void router.push({ path: `/flow/def/${row.id}/edit` });
}

// ==================== 发布 ====================

/** 当前操作行 */
const currentRow = ref<FlowDef | null>(null);
/** 发布弹窗可见 */
const publishVisible = ref(false);
/** 发布中 */
const publishing = ref(false);

/** 打开发布确认 */
function openPublish(row: FlowDef): void {
  currentRow.value = row;
  publishVisible.value = true;
}

/** 提交发布（失败显示后端原因：65010 / 65011 / 65015 …） */
async function submitPublish(): Promise<void> {
  if (!currentRow.value) {
    return;
  }
  publishing.value = true;
  try {
    const result = await publishFlowDef(currentRow.value.id);
    publishVisible.value = false;
    MessagePlugin.success(`已发布 v${result.version_no}（${result.node_count} 节点 / ${result.edge_count} 连线）`);
    await loadList();
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '发布失败');
  } finally {
    publishing.value = false;
  }
}

// ==================== 停用 ====================

/** 停用弹窗可见 */
const offlineVisible = ref(false);
/** 停用中 */
const offlining = ref(false);

/** 打开停用确认 */
function openOffline(row: FlowDef): void {
  currentRow.value = row;
  offlineVisible.value = true;
}

/** 提交停用 */
async function submitOffline(): Promise<void> {
  if (!currentRow.value) {
    return;
  }
  offlining.value = true;
  try {
    await offlineFlowDef(currentRow.value.id);
    offlineVisible.value = false;
    MessagePlugin.success('已停用（在跑审批单不受影响）');
    await loadList();
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '停用失败');
  } finally {
    offlining.value = false;
  }
}

// ==================== 删除 ====================

/** 删除弹窗可见 */
const deleteVisible = ref(false);
/** 删除中 */
const deleting = ref(false);

/** 打开删除确认 */
function openDelete(row: FlowDef): void {
  currentRow.value = row;
  deleteVisible.value = true;
}

/** 提交删除 */
async function submitDelete(): Promise<void> {
  if (!currentRow.value) {
    return;
  }
  deleting.value = true;
  try {
    const result = await deleteFlowDef(currentRow.value.id);
    deleteVisible.value = false;
    MessagePlugin.success(`已删除 ${result.flow_code}`);
    await loadList();
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '删除失败');
  } finally {
    deleting.value = false;
  }
}

// ==================== 版本历史 ====================

/** 版本抽屉可见 */
const versionsVisible = ref(false);
/** 版本列表 */
const versions = ref<FlowVersionSummary[]>([]);
/** 版本加载中 */
const versionLoading = ref(false);

/** 版本表列 */
const versionColumns: PrimaryTableCol[] = [
  { colKey: 'version_no', title: '版本', width: 110 },
  { colKey: 'version_status', title: '状态', width: 100 },
  { colKey: 'graph_size', title: '图规模', width: 130, cell: (_h, { row }) => `${row.node_count} / ${row.edge_count}` },
  { colKey: 'published_at', title: '发布时间', width: 160 },
  { colKey: 'published_by_name', title: '发布人', width: 110 },
  { colKey: 'op', title: '操作', width: 100 },
];

/** 打开版本历史 */
async function openVersions(row: FlowDef): Promise<void> {
  currentRow.value = row;
  versions.value = [];
  versionsVisible.value = true;
  versionLoading.value = true;
  try {
    const data = await listFlowVersions(row.id);
    versions.value = data.list ?? [];
  } catch (error) {
    MessagePlugin.error(error instanceof ApiError ? error.message : '版本列表加载失败');
  } finally {
    versionLoading.value = false;
  }
}

/** 从版本历史进编辑器（带 vid：看的就是这一版，PUBLISHED 版本进编辑器是只读） */
function openVersionEditor(row: FlowVersionSummary): void {
  if (!currentRow.value) {
    return;
  }
  versionsVisible.value = false;
  void router.push({ path: `/flow/def/${currentRow.value.id}/edit`, query: { vid: row.id } });
}

// ==================== 状态机边配置（D3 管理面入口） ====================

/** 边配置抽屉可见 */
const stateEdgeVisible = ref(false);
/** 打开时优先定位的实体（深链 `/flow/def?entity=STOCK_OUT` 或默认由后端顺序决定） */
const stateEdgeEntity = ref('');

/** 打开边配置面板（不传 entity 时由面板按后端返回顺序落到第一个实体） */
function openStateEdge(entity = ''): void {
  stateEdgeEntity.value = entity;
  stateEdgeVisible.value = true;
}

onMounted(() => {
  void loadList();
  // 深链：`/flow/def?entity=STOCK_OUT` 直接打开面板并定位到该实体（路由路径本身不动）
  const deepLink = route.query.entity;
  if (typeof deepLink === 'string' && deepLink.trim()) {
    openStateEdge(deepLink.trim());
  }
});
</script>

<style scoped lang="less">
.flow-def-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.filter-card,
.toolbar-card,
.table-card {
  border-radius: 8px;
}

.toolbar {
  display: flex;
  align-items: center;
  justify-content: space-between;

  &__count {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }
}

.table-alert {
  margin-bottom: 12px;
}

.cell-code {
  font-family: var(--app-font-mono, monospace);
  font-size: 13px;
  color: var(--td-text-color-secondary);
}

.cell-name {
  display: flex;
  gap: 8px;
  align-items: center;
}

.cell-muted {
  color: var(--td-text-color-placeholder);
}

.table-empty {
  display: flex;
  flex-direction: column;
  gap: 8px;
  align-items: center;
  padding: 32px 0;
  color: var(--td-text-color-placeholder);
}

.form-tip {
  margin-left: 8px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.dialog-tip {
  margin-top: 8px;
  font-size: 12px;
  line-height: 1.6;
  color: var(--td-text-color-secondary);
}

.drawer-loading {
  padding: 24px;
  color: var(--td-text-color-placeholder);
  text-align: center;
}

.version-current {
  margin-left: 6px;
}
</style>
