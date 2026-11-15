<!--
  互斥规则 / 职责分离 SoD（system/mutex）：规则列表 + 冲突查询 + 存量扫描（P3-13）。

  # 只有主表进 ListPage
  「冲突查询结果」与「存量扫描结果」是**一次查询的返回体**（几条、无分页、无翻页语义），
  按决策 §6 第 ① 问判为不进；主表是标准服务端分页只读行列表，进。

  # 删除入口只对「生效中」开放
  后端把「停用」当软删（`enabled=false` 即视为已删除），此后再 DELETE 返回 10002
  「互斥关系不存在或已停用」。所以停用行显示一个置灰 + tooltip 的假按钮，
  而不是留一个点了必然报错的入口（后端已验收，不改后端）。
-->
<template>
  <div class="mutex-page">
    <ListPage
      ref="listRef"
      title="互斥规则"
      meta-unit="条规则"
      testid="mutex"
      table-testid="mutex-table"
      meta-testid="mutex-meta"
      btn-prefix="btn-mutex"
      :page-size="MUTEX_PAGE_SIZE"
      :columns="columns"
      :filters="filters"
      :filter-state="filter"
      :fetcher="fetcher"
      :query-builder="queryBuilder"
    >
      <template #toolbar>
        <t-button
          v-privilege="[PERM.mutexCreate]"
          theme="primary"
          data-testid="btn-mutex-new"
          @click="openCreate"
        >
          <template #icon><t-icon name="add" /></template>
          新增互斥规则
        </t-button>
        <t-button
          v-privilege="[PERM.mutexConflict]"
          theme="default"
          :loading="scanLoading"
          data-testid="btn-mutex-scan"
          @click="runScan"
        >
          <template #icon><t-icon name="search" /></template>
          存量扫描
        </t-button>
      </template>

      <template #role_a="{ row }">
        <span class="cell-role">{{ row.role_a_name }}</span>
        <span class="cell-sub">{{ row.role_a_code }}</span>
      </template>
      <template #pair="{ row }">
        <t-icon class="pair-icon" name="swap" />
        <span class="pair-hint">{{ row.role_a_code }} &lt; {{ row.role_b_code }}</span>
      </template>
      <template #role_b="{ row }">
        <span class="cell-role">{{ row.role_b_name }}</span>
        <span class="cell-sub">{{ row.role_b_code }}</span>
      </template>
      <template #enabled="{ row }">
        <t-tag :theme="row.enabled ? 'success' : 'default'" variant="light" size="small">
          {{ row.enabled ? '生效中' : '已停用' }}
        </t-tag>
      </template>
      <template #created_at="{ row }">{{ formatDateTime(row.created_at) }}</template>
      <template #op="{ row }">
        <t-space size="small">
          <t-link
            v-privilege="[PERM.mutexCreate]"
            theme="primary"
            data-testid="btn-mutex-edit"
            @click.stop="openEdit(row)"
          >
            编辑
          </t-link>
          <t-link
            v-privilege="[PERM.mutexCreate]"
            :theme="row.enabled ? 'warning' : 'success'"
            data-testid="btn-mutex-toggle"
            @click.stop="onToggleEnabled(row, !row.enabled)"
          >
            {{ row.enabled ? '停用' : '启用' }}
          </t-link>
          <t-link
            v-if="row.enabled"
            v-privilege="[PERM.mutexDelete]"
            theme="danger"
            data-testid="btn-mutex-delete"
            @click.stop="confirmRemove(row)"
          >
            删除
          </t-link>
          <t-tooltip v-else v-privilege="[PERM.mutexDelete]" content="已停用规则在后端视为已删除，无需再次删除">
            <t-link theme="default" :disabled="true">删除</t-link>
          </t-tooltip>
        </t-space>
      </template>

      <template #empty>
        <div class="empty-state" data-testid="mutex-empty">
          <t-icon class="empty-state__icon" name="swap" />
          <p class="empty-state__title">还没有互斥规则</p>
          <p class="empty-state__desc">互斥 = 同一个人不能同时持有两个角色（职责分离 SoD）</p>
          <t-button v-privilege="[PERM.mutexCreate]" theme="primary" @click="openCreate">新增互斥规则</t-button>
        </div>
      </template>
    </ListPage>

    <!-- ========== 冲突查询（单用户体检） ========== -->
    <t-card v-if="canConflict" :bordered="false" class="conflict-card">
      <div class="section-head">
        <div class="section-head__title">
          <t-icon name="usergroup" />
          <span>冲突查询（单用户体检）</span>
        </div>
        <span class="section-head__hint">
          只读：不会改动任何用户角色，也不会让违规用户登录失败
        </span>
      </div>

      <t-space class="conflict-query">
        <t-select
          v-model="conflictUserId"
          class="conflict-query__select"
          placeholder="搜索并选择用户（支持用户名 / 姓名）"
          filterable
          clearable
          :loading="userLoading"
          :options="userOptions"
          data-testid="mutex-conflict-user"
          @search="onUserSearch"
        />
        <t-button
          theme="primary"
          :loading="conflictLoading"
          :disabled="!conflictUserId"
          data-testid="btn-mutex-conflict-query"
          @click="loadConflict"
        >
          查询冲突
        </t-button>
      </t-space>

      <div v-if="conflictResult" class="conflict-result">
        <div class="conflict-result__head">
          <span>
            用户 <strong>{{ conflictResult.display_name }}</strong>（{{ conflictResult.username }}）当前
            <t-tag
              :theme="conflictResult.conflicts.length > 0 ? 'danger' : 'success'"
              variant="light"
              size="small"
            >
              {{ conflictResult.conflicts.length > 0 ? `${conflictResult.conflicts.length} 组违规` : '未违规' }}
            </t-tag>
          </span>
        </div>
        <t-table
          v-if="conflictResult.conflicts.length > 0"
          row-key="mutex_id"
          size="small"
          :data="conflictResult.conflicts"
          :columns="conflictColumns"
          :bordered="false"
          data-testid="mutex-conflict-table"
        >
          <template #pair="{ row }">
            <t-tag theme="danger" variant="light" size="small">{{ row.role_a_name }}</t-tag>
            <span class="pair-hint"> ↔ </span>
            <t-tag theme="danger" variant="light" size="small">{{ row.role_b_name }}</t-tag>
          </template>
        </t-table>
        <div v-else class="conflict-empty">
          <t-icon name="check-circle" />
          <span>该用户没有命中任何生效中的互斥规则</span>
        </div>
      </div>
    </t-card>

    <!-- ========== 存量扫描结果 ========== -->
    <t-card v-if="scanResult" :bordered="false" class="scan-card">
      <div class="section-head">
        <div class="section-head__title">
          <t-icon name="chart-pie" />
          <span>存量扫描结果</span>
        </div>
        <t-link theme="primary" size="small" @click="scanResult = null">关闭</t-link>
      </div>

      <div class="scan-summary">
        <div class="scan-summary__item">
          <span class="scan-summary__label">扫描的生效规则</span>
          <span class="scan-summary__value">{{ scanResult.scanned_pairs }}</span>
        </div>
        <div class="scan-summary__item">
          <span class="scan-summary__label">存在违规的规则</span>
          <span class="scan-summary__value" :class="{ 'is-danger': scanResult.violated_pairs > 0 }">
            {{ scanResult.violated_pairs }}
          </span>
        </div>
        <div class="scan-summary__item">
          <span class="scan-summary__label">违规人次</span>
          <span class="scan-summary__value" :class="{ 'is-danger': scanResult.violated_holders > 0 }">
            {{ scanResult.violated_holders }}
          </span>
        </div>
      </div>

      <t-table
        row-key="mutex_id"
        :data="scanResult.items"
        :columns="scanColumns"
        size="small"
        :bordered="false"
        :empty="scanResult.items.length === 0 ? '本次扫描未发现违规组合' : ''"
        data-testid="mutex-scan-table"
      >
        <template #roles="{ row }">
          <t-tag size="small" variant="light" theme="default">{{ row.role_a_name }}</t-tag>
          <span class="pair-hint"> ↔ </span>
          <t-tag size="small" variant="light" theme="default">{{ row.role_b_name }}</t-tag>
        </template>
        <template #total="{ row }">
          <t-tag :theme="row.total > 0 ? 'danger' : 'success'" size="small" variant="light">
            {{ row.total }}
          </t-tag>
        </template>
        <template #sample="{ row }">
          <t-space v-if="row.sample.length > 0" size="small" break-line>
            <t-tag v-for="user in row.sample" :key="user.user_id" size="small" variant="outline">
              {{ user.display_name }}<template v-if="user.dept_name">（{{ user.dept_name }}）</template>
            </t-tag>
          </t-space>
          <span v-else class="cell-empty">—</span>
        </template>
      </t-table>
    </t-card>

    <!-- ========== 新增 / 编辑 ========== -->
    <t-dialog
      v-model:visible="formVisible"
      :header="isEdit ? '编辑互斥规则' : '新增互斥规则'"
      width="560px"
      :confirm-btn="{ content: isEdit ? '保存' : '创建', theme: 'primary', loading: submitting }"
      :cancel-btn="{ content: '取消' }"
      destroy-on-close
      data-testid="mutex-form-dialog"
      @confirm="handleSubmit"
    >
      <t-form :data="form" label-width="90px">
        <t-form-item label="角色 A" required>
          <t-select
            v-model="form.role_a_id"
            :disabled="isEdit"
            :options="roleSelectOptions"
            placeholder="选择第一个角色"
            filterable
            data-testid="mutex-form-role-a"
          />
        </t-form-item>
        <t-form-item label="角色 B" required>
          <t-select
            v-model="form.role_b_id"
            :disabled="isEdit"
            :options="roleSelectOptions"
            placeholder="选择第二个角色"
            filterable
            data-testid="mutex-form-role-b"
          />
          <div v-if="samePair" class="field-hint field-hint--error">
            A 与 B 不能是同一个角色（后端也会以 61012 拒绝）
          </div>
          <div v-else-if="isEdit" class="field-hint">角色对不可修改：要换角色请删除后重建</div>
        </t-form-item>
        <t-form-item label="说明" required>
          <t-textarea
            v-model="form.reason"
            :autosize="{ minRows: 3 }"
            :maxlength="200"
            placeholder="写清为什么这两个角色不能并存（如：制单与审核必须分离）"
            data-testid="mutex-form-reason"
          />
        </t-form-item>
      </t-form>
    </t-dialog>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { DialogPlugin, MessagePlugin } from 'tdesign-vue-next';
import type { SelectOption } from 'tdesign-vue-next';
import {
  createMutex,
  deleteMutex,
  getMutexConflict,
  pageMutexes,
  scanMutexes,
  updateMutex,
  type MutexConflictRes,
  type MutexRes,
  type MutexScanRes,
} from '@/api/business/mutex-api';
import { listRoles, type RoleRes } from '@/api/system/role-api';
import { pageUsers } from '@/api/system/user-api';
import { ApiError } from '@/api/request';
import { PERM } from '@/constants/permissions';
import { useUserStore } from '@/stores/user';
import { formatDateTime } from '@/utils/format';
import ListPage from '@/components/ListPage/index.vue';
import type {
  ColumnDef,
  FilterDef,
  ListFetcher,
  ListQuery,
  ListQueryBuilder,
} from '@/components/ListPage/types';

/** 每页条数（改造前是 10，保持手感一致） */
const MUTEX_PAGE_SIZE = 10;

const userStore = useUserStore();
/** 冲突查询 / 存量扫描 */
const canConflict = computed<boolean>(() => userStore.hasPermission(PERM.mutexConflict));

// ===== 列表 =====
/** 筛选状态（`status` 用 `''` 表示「全部」，1/0 才是真条件） */
interface MutexFilterState {
  /** 角色编码 / 名称 / 理由（模糊） */
  keyword: string;
  /** 1 生效中 / 0 已停用 / '' 全部 */
  status: number | '';
}

const filter = reactive<MutexFilterState>({ keyword: '', status: '' });
/** ListPage 实例（保存 / 启停 / 删除后重拉当前页） */
const listRef = ref<InstanceType<typeof ListPage> | null>(null);
const statusLoading = ref('');

const columns: ColumnDef[] = [
  { colKey: 'role_a', title: '角色 A', width: 170 },
  { colKey: 'pair', title: '规范化', width: 170 },
  { colKey: 'role_b', title: '角色 B', width: 170 },
  { colKey: 'reason', title: '说明', ellipsis: true },
  { colKey: 'enabled', title: '状态', width: 90 },
  { colKey: 'created_at', title: '创建时间', width: 170 },
  { colKey: 'op', title: '操作', width: 180, fixed: 'right', exportable: false },
];

/** 冲突查询结果表（一次查询的返回体，无分页） */
const conflictColumns: ColumnDef[] = [
  { colKey: 'pair', title: '违规组合', width: 260 },
  { colKey: 'reason', title: '互斥说明', ellipsis: true },
];

/** 存量扫描结果表（同上，无分页） */
const scanColumns: ColumnDef[] = [
  { colKey: 'roles', title: '互斥组合', width: 260 },
  { colKey: 'reason', title: '说明', ellipsis: true },
  { colKey: 'total', title: '违规人数', width: 100 },
  { colKey: 'sample', title: '违规用户（样本）', width: 340 },
];

const roleSelectOptions = ref<SelectOption[]>([]);

/** 筛选行：关键词 + 状态 */
const filters: FilterDef[] = [
  { key: 'keyword', type: 'keyword', placeholder: '角色编码 / 名称 / 理由（模糊）', width: 300, testid: 'mutex-filter-keyword' },
  {
    key: 'status',
    type: 'select',
    placeholder: '状态',
    width: 130,
    options: [
      { label: '生效中', value: 1 },
      { label: '已停用', value: 0 },
    ],
    testid: 'mutex-filter-status',
  },
];

/** 拉一页 */
const fetcher: ListFetcher<MutexRes> = (query) => pageMutexes(query as Parameters<typeof pageMutexes>[0]);

/** 入参拼装（键名与改造前逐字一致；`status = 0` 是真条件，必须发出去） */
const queryBuilder: ListQueryBuilder = (state, page, pageSize): ListQuery => {
  const { keyword, status } = state as MutexFilterState;
  const text = String(keyword ?? '').trim();
  return {
    page,
    page_size: pageSize,
    ...(text ? { keyword: text } : {}),
    ...(status === 0 || status === 1 ? { status } : {}),
  };
};

function showError(error: unknown, fallback: string): void {
  void MessagePlugin.error(error instanceof ApiError ? error.message || fallback : fallback);
}

/** 重拉当前页 */
async function reloadList(): Promise<void> {
  await listRef.value?.reload();
}

async function loadRoles(): Promise<void> {
  try {
    const list: RoleRes[] = await listRoles();
    roleSelectOptions.value = list.map((role) => ({
      label: `${role.role_name}（${role.role_code}）`,
      value: role.id,
    }));
  } catch {
    roleSelectOptions.value = [];
  }
}

// ===== 新增 / 编辑 =====
const formVisible = ref(false);
const isEdit = ref(false);
const editingId = ref('');
const submitting = ref(false);
const form = reactive<{ role_a_id: string; role_b_id: string; reason: string }>({
  role_a_id: '',
  role_b_id: '',
  reason: '',
});

/** A 与 B 是否相同（前端先拦一次，避免无意义的 61012 请求） */
const samePair = computed<boolean>(
  () => !!form.role_a_id && form.role_a_id === form.role_b_id,
);

function openCreate(): void {
  isEdit.value = false;
  editingId.value = '';
  form.role_a_id = '';
  form.role_b_id = '';
  form.reason = '';
  formVisible.value = true;
}

function openEdit(row: MutexRes): void {
  isEdit.value = true;
  editingId.value = row.id;
  form.role_a_id = row.role_a_id;
  form.role_b_id = row.role_b_id;
  form.reason = row.reason;
  formVisible.value = true;
}

async function handleSubmit(): Promise<void> {
  if (isEdit.value) {
    if (!form.reason.trim()) {
      void MessagePlugin.warning('说明必填');
      return;
    }
    submitting.value = true;
    try {
      await updateMutex(editingId.value, { reason: form.reason.trim() });
      void MessagePlugin.success('互斥规则已更新');
      formVisible.value = false;
      await reloadList();
    } catch (error) {
      showError(error, '保存互斥规则失败');
    } finally {
      submitting.value = false;
    }
    return;
  }

  if (!form.role_a_id || !form.role_b_id) {
    void MessagePlugin.warning('请选择两个角色');
    return;
  }
  if (samePair.value) {
    void MessagePlugin.warning('A 与 B 不能是同一个角色');
    return;
  }
  if (!form.reason.trim()) {
    void MessagePlugin.warning('说明必填');
    return;
  }
  await submitCreate(false);
}

/**
 * 创建规则；`force` 为 true 时带 `?force=true`（用户已确认接受存量冲突）。
 *
 * 后端失败码：61012 A==B / 61011 规则已存在（含反向）/ 61010 已有用户双持。
 */
async function submitCreate(force: boolean): Promise<void> {
  submitting.value = true;
  try {
    await createMutex(
      { role_a_id: form.role_a_id, role_b_id: form.role_b_id, reason: form.reason.trim() },
      force,
    );
    void MessagePlugin.success(force ? '规则已强制创建（存量违规需另行清理）' : '互斥规则已创建');
    formVisible.value = false;
    await reloadList();
  } catch (error) {
    if (error instanceof ApiError && error.code === 61010) {
      // 存量冲突：把后端点名的人数摆出来，由人决定是否强制
      const dialog = DialogPlugin.confirm({
        theme: 'warning',
        header: '已有用户同时持有这两个角色',
        body: error.message,
        confirmBtn: '仍然创建（记录违规）',
        cancelBtn: '先去清理用户',
        onConfirm: () => {
          dialog.destroy();
          void submitCreate(true);
        },
        onClose: () => dialog.destroy(),
      });
      return;
    }
    showError(error, '创建互斥规则失败');
  } finally {
    submitting.value = false;
  }
}

// ===== 启停 =====
function onToggleEnabled(row: MutexRes, enabled: boolean): void {
  if (enabled) {
    void applyEnabled(row, true);
    return;
  }
  // 停用 = 内控失效，PRD §6.2 要求二次确认
  const dialog = DialogPlugin.confirm({
    theme: 'warning',
    header: '确认停用该互斥规则？',
    body: `停用后「${row.role_a_name}」与「${row.role_b_name}」可以同时授予同一个人，互斥约束失效。`,
    confirmBtn: '确认停用',
    cancelBtn: '取消',
    onConfirm: () => {
      dialog.destroy();
      void applyEnabled(row, false);
    },
    onClose: () => dialog.destroy(),
  });
}

async function applyEnabled(row: MutexRes, enabled: boolean): Promise<void> {
  statusLoading.value = row.id;
  try {
    await updateMutex(row.id, { enabled });
    row.enabled = enabled;
    void MessagePlugin.success(enabled ? '规则已启用' : '规则已停用');
    // 以库为准重新拉取（启用会触发存量扫描，列表状态可能与本地不一致）
    await reloadList();
  } catch (error) {
    // 恢复启用时若有存量双持 → 409 + 61010，消息里有人数
    showError(error, '状态修改失败');
    await reloadList();
  } finally {
    statusLoading.value = '';
  }
}

// ===== 删除 =====
function confirmRemove(row: MutexRes): void {
  const dialog = DialogPlugin.confirm({
    theme: 'warning',
    header: '确认删除该互斥规则？',
    body: `将软删「${row.role_a_name}」↔「${row.role_b_name}」的互斥关系（说明：${row.reason}）。删除后内控无据可查，历史动作仍在审计日志中。`,
    confirmBtn: '确认删除',
    cancelBtn: '取消',
    onConfirm: async () => {
      dialog.destroy();
      try {
        await deleteMutex(row.id);
        void MessagePlugin.success('互斥规则已删除');
        await reloadList();
      } catch (error) {
        showError(error, '删除失败');
      }
    },
    onClose: () => dialog.destroy(),
  });
}

// ===== 冲突查询 =====
const conflictUserId = ref('');
const userOptions = ref<SelectOption[]>([]);
const userLoading = ref(false);
const conflictLoading = ref(false);
const conflictResult = ref<MutexConflictRes | null>(null);

/** 远程搜索用户（按用户名 / 姓名模糊匹配） */
async function onUserSearch(keyword: string): Promise<void> {
  userLoading.value = true;
  try {
    const data = await pageUsers({ page: 1, page_size: 20, username: keyword || undefined });
    userOptions.value = data.list.map((user) => ({
      label: `${user.real_name || user.username}（${user.username}）`,
      value: user.id,
    }));
  } catch {
    userOptions.value = [];
  } finally {
    userLoading.value = false;
  }
}

async function loadConflict(): Promise<void> {
  if (!conflictUserId.value) {
    return;
  }
  conflictLoading.value = true;
  try {
    conflictResult.value = await getMutexConflict(conflictUserId.value);
  } catch (error) {
    conflictResult.value = null;
    showError(error, '冲突查询失败');
  } finally {
    conflictLoading.value = false;
  }
}

// ===== 存量扫描 =====
const scanLoading = ref(false);
const scanResult = ref<MutexScanRes | null>(null);

async function runScan(): Promise<void> {
  scanLoading.value = true;
  try {
    scanResult.value = await scanMutexes();
    const { violated_pairs, violated_holders } = scanResult.value;
    if (violated_pairs === 0) {
      void MessagePlugin.success('扫描完成：未发现违规组合');
    } else {
      void MessagePlugin.warning(
        `扫描完成：${violated_pairs} 条规则存在违规，共 ${violated_holders} 人次`,
      );
    }
  } catch (error) {
    scanResult.value = null;
    showError(error, '存量扫描失败');
  } finally {
    scanLoading.value = false;
  }
}

onMounted(() => {
  void loadRoles();
  void onUserSearch('');
});
</script>

<style lang="less" scoped>
.mutex-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.conflict-card,
.scan-card {
  :deep(.t-card__body) {
    padding: 16px;
  }
}

.cell-role {
  font-weight: 600;
}

.cell-sub {
  margin-left: 8px;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.cell-empty {
  color: var(--td-text-color-placeholder);
}

.pair-icon {
  margin-right: 4px;
  color: var(--td-text-color-placeholder);
}

.pair-hint {
  font-size: 12px;
  color: var(--td-text-color-secondary);
}

.section-head {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  margin-bottom: 12px;

  &__title {
    display: flex;
    align-items: center;
    gap: 6px;
    font-weight: 600;
    color: var(--td-text-color-primary);
  }

  &__hint {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }
}

.conflict-query {
  margin-bottom: 12px;

  &__select {
    width: 320px;
  }
}

.conflict-result {
  padding-top: 12px;
  border-top: 1px solid var(--td-component-stroke);

  &__head {
    margin-bottom: 8px;
    font-size: 13px;
    color: var(--td-text-color-secondary);

    strong {
      color: var(--td-text-color-primary);
    }
  }
}

.conflict-empty {
  display: flex;
  align-items: center;
  gap: 6px;
  padding: 12px 14px;
  border-radius: 6px;
  font-size: 13px;
  color: var(--td-success-color);
  background: var(--td-bg-color-page);
}

.scan-summary {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
  gap: 12px;
  margin-bottom: 12px;

  &__item {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding: 10px 12px;
    border: 1px solid var(--td-component-stroke);
    border-radius: 6px;
    background: var(--td-bg-color-page);
  }

  &__label {
    font-size: 12px;
    color: var(--td-text-color-secondary);
  }

  &__value {
    font-size: 20px;
    font-weight: 600;
    color: var(--td-text-color-primary);

    &.is-danger {
      color: var(--td-error-color);
    }
  }
}

.field-hint {
  margin-top: 4px;
  font-size: 12px;
  line-height: 1.6;
  color: var(--td-text-color-secondary);

  &--error {
    color: var(--td-error-color);
  }
}

.empty-state {
  padding: 40px 0;
  text-align: center;

  &__icon {
    font-size: 44px;
    color: var(--td-text-color-placeholder);
  }

  &__title {
    margin: 12px 0 4px;
    font-size: 15px;
    color: var(--td-text-color-primary);
  }

  &__desc {
    margin-bottom: 14px;
    font-size: 13px;
    color: var(--td-text-color-secondary);
  }
}
</style>
