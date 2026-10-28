<template>
  <div class="scope-panel" :class="{ 'scope-panel--embedded': embedded }">
    <t-loading :loading="loading" size="small" text="加载数据范围配置...">
      <!-- ========== 当前生效配置（后端下发的标签，前端不硬编码） ========== -->
      <div class="scope-current">
        <div class="scope-current__main">
          <span class="scope-current__label">当前生效范围</span>
          <t-tag v-if="scope" theme="primary" variant="light" size="medium">
            {{ scope.data_scope_label }}
          </t-tag>
          <span v-else class="scope-current__empty">未读取到配置</span>
          <t-tag v-if="scope" theme="default" variant="outline" size="small">
            {{ scope.data_scope_code }}
          </t-tag>
        </div>
        <div v-if="scope" class="scope-current__meta">
          持有者 <strong>{{ scope.holder_count }}</strong> 人 · 保存后这些人的数据范围缓存立即失效
        </div>
      </div>

      <!-- 自定义部门回显（存量配置：即使当前范围不是 5 也照实展示） -->
      <div v-if="scope && scope.custom_depts.length > 0" class="scope-saved-depts">
        <span class="scope-saved-depts__label">库里已保存的自定义部门</span>
        <t-space size="small" break-line>
          <t-tag
            v-for="dept in scope.custom_depts"
            :key="dept.id"
            size="small"
            variant="light"
            theme="default"
          >
            {{ dept.dept_name }}
          </t-tag>
        </t-space>
      </div>

      <!-- ========== 错误态：61002 越界 / 61003 自持 / 61001 参数 ========== -->
      <t-alert v-if="errorText" class="scope-error" theme="error" :message="errorText">
        <template #title>
          保存被拒绝（{{ errorCode ? `错误码 ${errorCode}` : '请求失败' }}）
        </template>
        <template #operation>
          <t-link theme="primary" size="small" @click="errorText = ''">知道了</t-link>
        </template>
      </t-alert>

      <!-- ========== 5 种范围单选 ========== -->
      <div class="scope-section">
        <div class="scope-section__title">
          数据范围
          <span v-if="!canWrite" class="scope-section__readonly">（只读：缺少权限码 business:permission:scope:write）</span>
        </div>
        <t-radio-group
          v-model="form.data_scope"
          class="scope-radios"
          :disabled="!canWrite || saving"
          @change="onScopeChange"
        >
          <t-radio v-for="option in DATA_SCOPE_OPTIONS" :key="option.value" :value="option.value">
            <span class="scope-radio__label">{{ option.label }}</span>
            <span class="scope-radio__hint">{{ option.hint }}</span>
          </t-radio>
        </t-radio-group>
      </div>

      <!-- ========== 自定义部门树（仅「自定义部门」展开） ========== -->
      <div v-if="form.data_scope === 5" class="scope-section scope-section--custom">
        <div class="scope-section__title">
          自定义部门
          <span class="scope-section__tip">
            数据源为 <code>GET /api/business/permission/scope/dept-tree</code>，已由后端按操作者范围裁剪
            <template v-if="treeInfo">
              （本次返回 {{ treeInfo.total }} 个部门<template v-if="treeInfo.scope_limited">，范围受限</template>）
            </template>
          </span>
        </div>

        <t-loading :loading="treeLoading" size="small" text="加载部门树...">
          <div class="scope-tree">
            <div class="scope-tree__toolbar">
              <t-space size="small">
                <t-button size="small" variant="outline" @click="expandAll = !expandAll">
                  <template #icon>
                    <t-icon :name="expandAll ? 'chevron-up' : 'chevron-down'" />
                  </template>
                  {{ expandAll ? '折叠全部' : '展开全部' }}
                </t-button>
                <t-button size="small" variant="outline" @click="selectAllDepts">全选可见部门</t-button>
                <t-button size="small" variant="outline" @click="clearDepts">清空</t-button>
              </t-space>
              <span class="scope-tree__count">
                已选 <strong>{{ checkedDeptIds.length }}</strong> 个部门
              </span>
            </div>
            <div class="scope-tree__body">
              <t-tree
                :data="treeData"
                :keys="TREE_KEYS"
                :value="checkedDeptIds"
                :expand-all="expandAll"
                checkable
                hover
                @change="onTreeChange"
              />
              <div v-if="!treeLoading && treeData.length === 0" class="scope-tree__empty">
                后端未返回任何可选部门（你的可见范围为空，无法配置自定义部门）
              </div>
            </div>
          </div>
        </t-loading>

        <!-- 已选部门的名称清单：折叠树里也能确认勾了哪些 -->
        <div v-if="checkedDeptNames.length > 0" class="scope-selected">
          <t-space size="small" break-line>
            <t-tag
              v-for="item in checkedDeptNames"
              :key="item.id"
              theme="primary"
              variant="light"
              size="small"
            >
              {{ item.name }}
            </t-tag>
          </t-space>
        </div>
      </div>

      <!-- ========== 操作区 ========== -->
      <div class="scope-footer">
        <t-space>
          <t-button
            v-if="canWrite"
            theme="primary"
            :loading="saving"
            :disabled="!dirty"
            @click="handleSave"
          >
            保存数据范围
          </t-button>
          <t-button v-if="canWrite" theme="default" :disabled="saving" @click="resetForm">
            重置
          </t-button>
          <t-button
            v-if="canWrite && scope && scope.custom_depts.length > 0 && scope.data_scope !== 5"
            theme="default"
            variant="outline"
            :loading="saving"
            @click="handleClearCustom"
          >
            清空自定义部门（{{ scope.custom_depts.length }}）
          </t-button>
        </t-space>
        <span v-if="!canWrite" class="scope-footer__hint">
          无写入权限：只能查看，按钮已隐藏
        </span>
        <span v-else-if="!dirty" class="scope-footer__hint">配置无变化</span>
      </div>
    </t-loading>
  </div>
</template>

<script setup lang="ts">
import { computed, ref, watch } from 'vue';
import { DialogPlugin, MessagePlugin } from 'tdesign-vue-next';
import {
  DATA_SCOPE_OPTIONS,
  getRoleScope,
  getScopeDeptTree,
  updateRoleScope,
  type RoleScopeRes,
  type ScopeDeptTreeRes,
  type SetRoleScopeParams,
} from '@/api/business/permission-scope-api';
import { ApiError } from '@/api/request';
import { PERM } from '@/constants/permissions';
import { useUserStore } from '@/stores/user';

const props = withDefaults(
  defineProps<{
    /** 目标角色 ID */
    roleId: string;
    /** 目标角色名称（二次确认文案用） */
    roleName?: string;
    /** 嵌入其它容器时去掉外层留白 */
    embedded?: boolean;
  }>(),
  { roleName: '', embedded: false },
);

const emit = defineEmits<{
  /** 保存成功（携带后端返回的生效配置） */
  (e: 'saved'): void;
}>();

const userStore = useUserStore();
/** 是否可写（无此码时保存按钮隐藏，只读展示） */
const canWrite = computed(() => userStore.hasPermission(PERM.scopeWrite));

/** 部门树字段映射：id → value，dept_name → label */
const TREE_KEYS = { value: 'id', label: 'dept_name', children: 'children' } as const;

// ===== 当前配置 =====
const scope = ref<RoleScopeRes | null>(null);
const loading = ref(false);
/** 表单：待提交的范围与自定义部门 */
const form = ref<{ data_scope: number; custom_dept_ids: string[] }>({
  data_scope: 4,
  custom_dept_ids: [],
});
/** 已勾选部门 ID（树组件绑定值） */
const checkedDeptIds = ref<string[]>([]);

// ===== 部门树 =====
const treeData = ref<ScopeDeptTreeRes['tree']>([]);
const treeInfo = ref<ScopeDeptTreeRes | null>(null);
const treeLoading = ref(false);
const expandAll = ref(true);
/** 部门 ID → 名称（已选标签 + 越界定位都用它） */
const deptNameMap = ref<Record<string, string>>({});

// ===== 状态 =====
const saving = ref(false);
const errorText = ref('');
const errorCode = ref(0);

/** 表单是否被改动过（未改动时禁用保存，避免无意义缓存失效） */
const dirty = computed(() => {
  if (!scope.value) {
    return false;
  }
  const scopeChanged = Number(scope.value.data_scope) !== Number(form.value.data_scope);
  const savedIds = [...scope.value.custom_dept_ids].sort().join(',');
  const formIds = [...form.value.custom_dept_ids].sort().join(',');
  return scopeChanged || savedIds !== formIds;
});

/** 已选部门的名称清单（按树顺序） */
const checkedDeptNames = computed<Array<{ id: string; name: string }>>(() =>
  checkedDeptIds.value.map((id) => ({ id, name: deptNameMap.value[id] ?? id })),
);

/** 把树拍平，建立 ID → 名称映射 */
function indexTree(nodes: ScopeDeptTreeRes['tree']): void {
  for (const node of nodes) {
    deptNameMap.value[node.id] = node.dept_name;
    if (node.children.length > 0) {
      indexTree(node.children);
    }
  }
}

/** 收集全部节点 ID（全选按钮用；只收集后端允许可见的节点） */
function collectTreeIds(nodes: ScopeDeptTreeRes['tree'], out: string[] = []): string[] {
  for (const node of nodes) {
    out.push(node.id);
    collectTreeIds(node.children, out);
  }
  return out;
}

/** 统一错误展示：保留后端原文（61002 会点名越界部门，绝不能吞） */
function showError(error: unknown, fallback: string): void {
  if (error instanceof ApiError) {
    errorCode.value = error.code;
    errorText.value = error.message || fallback;
    void MessagePlugin.error(error.message || fallback);
    return;
  }
  errorCode.value = 0;
  errorText.value = fallback;
  void MessagePlugin.error(fallback);
}

/** 读取角色当前数据范围 */
async function loadScope(): Promise<void> {
  if (!props.roleId) {
    scope.value = null;
    return;
  }
  loading.value = true;
  errorText.value = '';
  try {
    const data = await getRoleScope(props.roleId);
    scope.value = data;
    form.value = {
      data_scope: Number(data.data_scope),
      custom_dept_ids: [...data.custom_dept_ids],
    };
    checkedDeptIds.value = [...data.custom_dept_ids];
  } catch (error) {
    scope.value = null;
    showError(error, '数据范围配置加载失败');
  } finally {
    loading.value = false;
  }
}

/** 加载（已裁剪的）部门树 */
async function loadTree(): Promise<void> {
  if (treeData.value.length > 0) {
    return;
  }
  treeLoading.value = true;
  try {
    const data = await getScopeDeptTree();
    treeInfo.value = data;
    treeData.value = data.tree;
    deptNameMap.value = {};
    indexTree(data.tree);
  } catch (error) {
    treeData.value = [];
    showError(error, '部门树加载失败');
  } finally {
    treeLoading.value = false;
  }
}

/** 重置表单为后端回显值 */
function resetForm(): void {
  if (!scope.value) {
    return;
  }
  form.value = {
    data_scope: Number(scope.value.data_scope),
    custom_dept_ids: [...scope.value.custom_dept_ids],
  };
  checkedDeptIds.value = [...scope.value.custom_dept_ids];
  errorText.value = '';
}

/** 范围切换：切到「自定义」时按需拉树 */
function onScopeChange(): void {
  if (form.value.data_scope === 5) {
    form.value.custom_dept_ids = [...checkedDeptIds.value];
    void loadTree();
  }
}

/** 树勾选变化（TDesign 的 value 为受控勾选集合） */
function onTreeChange(value: unknown): void {
  const ids = Array.isArray(value) ? value.map((item) => String(item)) : [];
  checkedDeptIds.value = ids;
  form.value.custom_dept_ids = [...ids];
}

function selectAllDepts(): void {
  checkedDeptIds.value = collectTreeIds(treeData.value);
  form.value.custom_dept_ids = [...checkedDeptIds.value];
}

function clearDepts(): void {
  checkedDeptIds.value = [];
  form.value.custom_dept_ids = [];
}

/** 真正提交（二次确认后调用） */
async function submit(): Promise<void> {
  if (!props.roleId) {
    return;
  }
  saving.value = true;
  errorText.value = '';
  try {
    // 后端三态语义：非「自定义」范围**不能**带非空集合（61001），所以只在 data_scope=5 时
    // 提交集合；其它范围省略该字段 = 不动库里的存量自定义部门（清空请用「清空自定义部门」）。
    const payload: SetRoleScopeParams = { data_scope: Number(form.value.data_scope) };
    if (form.value.data_scope === 5) {
      payload.custom_dept_ids = [...form.value.custom_dept_ids];
    }
    const res = await updateRoleScope(props.roleId, payload);
    scope.value = {
      ...(scope.value as RoleScopeRes),
      data_scope: res.data_scope,
      data_scope_code: res.data_scope_code,
      data_scope_label: res.data_scope_label,
      custom_dept_ids: res.custom_dept_ids,
      custom_depts: res.custom_depts,
      role_id: res.role_id,
    };
    form.value = {
      data_scope: Number(res.data_scope),
      custom_dept_ids: [...res.custom_dept_ids],
    };
    checkedDeptIds.value = [...res.custom_dept_ids];
    void MessagePlugin.success(
      `数据范围已保存为「${res.data_scope_label}」，${res.invalidated_users} 位持有者的缓存已失效`,
    );
    emit('saved');
    // 回读一次，确保界面与库侧一致（并刷新 holder_count）
    await loadScope();
  } catch (error) {
    showError(error, '保存数据范围失败');
  } finally {
    saving.value = false;
  }
}

/**
 * 保存入口：切到「全部数据」时按 PRD §6.2 必须二次确认
 * （改 → ALL 是最高风险动作：一次误配即全员可见全公司数据）。
 */
function handleSave(): void {
  if (form.value.data_scope === 5 && form.value.custom_dept_ids.length === 0) {
    errorCode.value = 61001;
    errorText.value = '数据范围为「自定义部门」时必须至少选择一个部门';
    void MessagePlugin.warning('请先勾选至少一个部门');
    return;
  }
  const current = scope.value ? Number(scope.value.data_scope) : 0;
  if (form.value.data_scope === 1 && current !== 1) {
    const dialog = DialogPlugin.confirm({
      theme: 'warning',
      header: '确认放开为「全部数据」？',
      body: `保存后角色「${props.roleName || props.roleId}」的 ${scope.value?.holder_count ?? 0} 位持有者将不受部门限制，可看到全部数据。`,
      confirmBtn: '确认放开',
      cancelBtn: '取消',
      onConfirm: () => {
        dialog.destroy();
        void submit();
      },
      onClose: () => dialog.destroy(),
    });
    return;
  }
  void submit();
}

/** 清空库里的自定义部门集合（保留当前范围形态：传 data_scope 当前值 + 空集合） */
async function handleClearCustom(): Promise<void> {
  if (!props.roleId || !scope.value) {
    return;
  }
  const dialog = DialogPlugin.confirm({
    theme: 'warning',
    header: '确认清空自定义部门？',
    body: `将删除该角色已保存的 ${scope.value.custom_depts.length} 个自定义部门（当前范围「${scope.value.data_scope_label}」不变）。`,
    confirmBtn: '确认清空',
    cancelBtn: '取消',
    onConfirm: async () => {
      dialog.destroy();
      saving.value = true;
      errorText.value = '';
      try {
        const res = await updateRoleScope(props.roleId, {
          data_scope: Number(scope.value?.data_scope ?? 4),
          custom_dept_ids: [],
        });
        void MessagePlugin.success(`自定义部门已清空（${res.invalidated_users} 位持有者缓存失效）`);
        emit('saved');
        await loadScope();
      } catch (error) {
        showError(error, '清空自定义部门失败');
      } finally {
        saving.value = false;
      }
    },
    onClose: () => dialog.destroy(),
  });
}

watch(
  () => props.roleId,
  () => {
    checkedDeptIds.value = [];
    void loadScope();
  },
  { immediate: true },
);
</script>

<style lang="less" scoped>
.scope-panel {
  padding: 4px 2px;

  &--embedded {
    padding: 0;
  }
}

.scope-current {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px 12px;
  margin-bottom: 12px;
  padding: 10px 12px;
  border: 1px solid var(--td-component-stroke);
  border-radius: 6px;
  background: var(--td-bg-color-page);

  &__main {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  &__label {
    font-size: 13px;
    color: var(--td-text-color-secondary);
  }

  &__empty {
    color: var(--td-text-color-placeholder);
  }

  &__meta {
    font-size: 12px;
    color: var(--td-text-color-secondary);

    strong {
      color: var(--td-brand-color);
    }
  }
}

.scope-saved-depts {
  margin-bottom: 12px;
  font-size: 12px;

  &__label {
    display: block;
    margin-bottom: 6px;
    color: var(--td-text-color-secondary);
  }
}

.scope-error {
  margin-bottom: 12px;
}

.scope-section {
  margin-bottom: 16px;

  &__title {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 8px;
    margin-bottom: 8px;
    font-size: 13px;
    font-weight: 600;
    color: var(--td-text-color-primary);
  }

  &__tip {
    font-weight: 400;
    font-size: 12px;
    color: var(--td-text-color-secondary);

    code {
      padding: 0 4px;
      border-radius: 2px;
      background: var(--td-bg-color-secondarycontainer);
    }
  }

  &__readonly {
    font-weight: 400;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }
}

.scope-radios {
  display: flex;
  flex-direction: column;
  gap: 6px;

  :deep(.t-radio) {
    display: flex;
    align-items: baseline;
  }
}

.scope-radio {
  &__label {
    color: var(--td-text-color-primary);
  }

  &__hint {
    margin-left: 8px;
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }
}

.scope-tree {
  border: 1px solid var(--td-component-stroke);
  border-radius: 6px;

  &__toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 8px 12px;
    border-bottom: 1px solid var(--td-component-stroke);
    background: var(--td-bg-color-container);
  }

  &__count {
    font-size: 12px;
    color: var(--td-text-color-secondary);

    strong {
      color: var(--td-brand-color);
    }
  }

  &__body {
    max-height: 280px;
    padding: 8px 12px;
    overflow-y: auto;
  }

  &__empty {
    padding: 24px 0;
    text-align: center;
    font-size: 13px;
    color: var(--td-text-color-placeholder);
  }
}

.scope-selected {
  margin-top: 8px;
  padding: 8px 12px;
  border-radius: 6px;
  background: var(--td-bg-color-page);
}

.scope-footer {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  padding-top: 12px;
  border-top: 1px solid var(--td-component-stroke);

  &__hint {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }
}
</style>
