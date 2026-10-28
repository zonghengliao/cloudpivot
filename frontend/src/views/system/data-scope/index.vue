<template>
  <div class="data-scope-page">
    <!-- ========== 我的数据范围自检（权限码 business:permission:scope:self-check） ========== -->
    <t-card v-if="canSelfCheck" :bordered="false" class="check-card">
      <div class="check-card__head">
        <div class="check-card__title">
          <t-icon name="lock-on" />
          <span>我的数据范围自检</span>
        </div>
        <t-button
          size="small"
          variant="outline"
          :loading="checkLoading"
          @click="loadSelfCheck"
        >
          <template #icon><t-icon name="refresh" /></template>
          重新体检
        </t-button>
      </div>

      <t-loading :loading="checkLoading" size="small">
        <template v-if="selfCheck">
          <div class="check-grid">
            <div class="check-item">
              <span class="check-item__label">生效范围</span>
              <span class="check-item__value">
                <t-tag theme="primary" variant="light" size="small">{{ selfCheck.scope.label }}</t-tag>
                <span class="check-item__sub">{{ selfCheck.scope.code }}</span>
              </span>
            </div>
            <div class="check-item">
              <span class="check-item__label">可见部门</span>
              <span class="check-item__value">
                {{ selfCheck.dept_count }} 个
                <span v-if="selfCheck.self_only" class="check-item__sub">（并含「仅本人」的行）</span>
              </span>
            </div>
            <div class="check-item">
              <span class="check-item__label">所属部门</span>
              <span class="check-item__value">{{ selfCheck.own_dept_name || '未分配部门' }}</span>
            </div>
            <div class="check-item">
              <span class="check-item__label">超管</span>
              <span class="check-item__value">{{ selfCheck.is_super_admin ? '是（不受数据范围限制）' : '否' }}</span>
            </div>
          </div>

          <div class="check-roles">
            <span class="check-roles__label">范围来源角色</span>
            <t-space size="small" break-line>
              <t-tag v-for="role in selfCheck.roles" :key="role.role_id" size="small" variant="outline">
                {{ role.role_name }}（{{ role.data_scope_code }}）
              </t-tag>
              <span v-if="selfCheck.roles.length === 0" class="check-item__sub">无角色（默认仅本人）</span>
            </t-space>
          </div>

          <div class="check-tables">
            <div class="check-tables__head">
              <span class="check-tables__title">各登记表最终注入的过滤片段（不含参数值）</span>
              <t-link theme="primary" size="small" @click="showTables = !showTables">
                {{ showTables ? '收起' : `展开（${selfCheck.tables.length} 张表）` }}
              </t-link>
            </div>
            <t-table
              v-if="showTables"
              row-key="table"
              size="small"
              :data="selfCheck.tables"
              :columns="filterColumns"
              :bordered="false"
            >
              <template #is_noop="{ row }">
                <t-tag :theme="row.is_noop ? 'default' : 'success'" size="small" variant="light">
                  {{ row.is_noop ? '不过滤' : '已过滤' }}
                </t-tag>
              </template>
              <template #degraded="{ row }">
                <t-tag v-if="row.degraded" theme="warning" size="small" variant="light">降级</t-tag>
                <span v-else>-</span>
              </template>
            </t-table>
          </div>
        </template>
        <div v-else class="check-empty">未能读取自检结果</div>
      </t-loading>
    </t-card>

    <!-- ========== 角色数据范围一览 ========== -->
    <t-card :bordered="false" class="filter-card">
      <t-row :gutter="16">
        <t-col :span="5">
          <t-input
            v-model="query.role_name"
            placeholder="角色名称（模糊）"
            clearable
            @enter="handleSearch"
          />
        </t-col>
        <t-col :span="7">
          <t-space>
            <t-button theme="primary" @click="handleSearch">
              <template #icon><t-icon name="search" /></template>
              查询
            </t-button>
            <t-button theme="default" @click="handleReset">重置</t-button>
            <t-button theme="default" :loading="loading" @click="loadList">
              <template #icon><t-icon name="refresh" /></template>
              刷新
            </t-button>
          </t-space>
        </t-col>
      </t-row>
    </t-card>

    <t-card :bordered="false" class="table-card">
      <div class="toolbar">
        <t-space>
          <span class="toolbar__title">角色数据范围</span>
          <t-tag size="small" variant="light" theme="default">共 {{ pagination.total }} 个角色</t-tag>
        </t-space>
        <span class="toolbar__hint">
          数据范围是角色的属性（不是独立实体）：修改即「编辑」，清空自定义部门集合即「删除」子配置
        </span>
      </div>

      <t-table
        row-key="id"
        :data="rows"
        :columns="columns"
        :loading="loading"
        :pagination="pagination"
        stripe
        @page-change="handlePageChange"
      >
        <template #role="{ row }">
          <span class="cell-name">{{ row.role_name }}</span>
          <span class="cell-sub">{{ row.role_code }}</span>
        </template>
        <template #scope="{ row }">
          <t-tag v-if="row.scope" theme="primary" variant="light" size="small">
            {{ row.scope.data_scope_label }}
          </t-tag>
          <t-tag v-else theme="danger" variant="light" size="small">读取失败</t-tag>
        </template>
        <template #depts="{ row }">
          <t-space v-if="row.scope && row.scope.custom_depts.length > 0" size="small" break-line>
            <t-tag v-for="dept in row.scope.custom_depts" :key="dept.id" size="small" variant="outline">
              {{ dept.dept_name }}
            </t-tag>
          </t-space>
          <span v-else class="cell-empty">—</span>
        </template>
        <template #holders="{ row }">{{ row.scope ? row.scope.holder_count : '-' }}</template>
        <template #op="{ row }">
          <t-space size="small">
            <t-link v-if="canWrite" theme="primary" @click="openConfig(row)">配置</t-link>
            <t-link v-else theme="default" @click="openConfig(row)">查看</t-link>
          </t-space>
        </template>
        <template #empty>
          <div class="empty-state">
            <t-icon class="empty-state__icon" name="lock-on" />
            <p class="empty-state__title">暂无角色</p>
            <p class="empty-state__desc">数据范围挂在角色上，请先在「角色管理」创建角色</p>
          </div>
        </template>
      </t-table>
    </t-card>

    <!-- ========== 配置抽屉：复用角色编辑页同一块面板 ========== -->
    <t-drawer
      v-model:visible="drawerVisible"
      :header="`数据权限 · ${current?.role_name ?? ''}`"
      size="640px"
      :footer="false"
      destroy-on-close
    >
      <data-scope-panel
        v-if="current"
        :role-id="current.id"
        :role-name="current.role_name"
        @saved="onSaved"
      />
    </t-drawer>
  </div>
</template>

<script setup lang="ts">
import { computed, onMounted, reactive, ref } from 'vue';
import { MessagePlugin } from 'tdesign-vue-next';
import type { PrimaryTableCol } from 'tdesign-vue-next';
import { pageRoles, type RoleRes } from '@/api/system/role-api';
import {
  getRoleScope,
  getScopeSelfCheck,
  type RoleScopeRes,
  type ScopeSelfCheckRes,
} from '@/api/business/permission-scope-api';
import { ApiError } from '@/api/request';
import { PERM } from '@/constants/permissions';
import { useUserStore } from '@/stores/user';
import DataScopePanel from '@/views/system/role/components/DataScopePanel.vue';

/** 表格分页回调参数（TDesign 未导出该类型，与项目其它页面同一口径） */
interface TablePageInfo {
  /** 当前页 */
  current: number;
  /** 每页条数 */
  pageSize: number;
}

/** 列表行 = 角色 + 它的数据范围配置（配置可能读不到，用 null 表示） */
interface ScopeRow extends RoleRes {
  /** 该角色的数据范围配置 */
  scope: RoleScopeRes | null;
}

const userStore = useUserStore();
/** 可写（保存 / 删除子配置） */
const canWrite = computed<boolean>(() => userStore.hasPermission(PERM.scopeWrite));
/** 可体检 */
const canSelfCheck = computed<boolean>(() => userStore.hasPermission(PERM.scopeSelfCheck));

// ===== 列表 =====
const loading = ref(false);
const rows = ref<ScopeRow[]>([]);
const query = reactive({ role_name: '' });
const pagination = reactive({ current: 1, pageSize: 10, total: 0 });

const columns: PrimaryTableCol[] = [
  { colKey: 'role', title: '角色', width: 200 },
  { colKey: 'scope', title: '数据范围', width: 140 },
  { colKey: 'depts', title: '自定义部门', width: 260 },
  { colKey: 'holders', title: '持有者', width: 90, align: 'right' },
  { colKey: 'op', title: '操作', width: 90, fixed: 'right' },
];

// ===== 自检 =====
const selfCheck = ref<ScopeSelfCheckRes | null>(null);
const checkLoading = ref(false);
/** 是否展开「注入片段」明细表 */
const showTables = ref(false);
const filterColumns: PrimaryTableCol[] = [
  { colKey: 'table', title: '表', width: 180 },
  { colKey: 'alias', title: '别名', width: 70 },
  { colKey: 'where_sql', title: '注入片段', ellipsis: true },
  { colKey: 'binds', title: '参数', width: 70, align: 'right' },
  { colKey: 'is_noop', title: '生效', width: 90 },
  { colKey: 'degraded', title: '降级', width: 80 },
];

// ===== 抽屉 =====
const drawerVisible = ref(false);
const current = ref<RoleRes | null>(null);

/** 统一错误提示（保留后端原文） */
function showError(error: unknown, fallback: string): void {
  void MessagePlugin.error(error instanceof ApiError ? error.message || fallback : fallback);
}

/** 加载角色列表 + 逐行回填数据范围（后端没有「带范围的角色列表」接口，故并发取详情） */
async function loadList(): Promise<void> {
  loading.value = true;
  try {
    const data = await pageRoles({
      page: pagination.current,
      page_size: pagination.pageSize,
      role_name: query.role_name || undefined,
    });
    pagination.total = data.total;
    const detailed = await Promise.all(
      data.list.map(async (role) => {
        try {
          const scope = await getRoleScope(role.id);
          return { ...role, scope } as ScopeRow;
        } catch {
          // 单个角色读失败不拖垮整张表（比如并发被回收的角色）
          return { ...role, scope: null } as ScopeRow;
        }
      }),
    );
    rows.value = detailed;
  } catch (error) {
    rows.value = [];
    pagination.total = 0;
    showError(error, '角色数据范围加载失败');
  } finally {
    loading.value = false;
  }
}

/** 当前用户的数据权限自检 */
async function loadSelfCheck(): Promise<void> {
  if (!canSelfCheck.value) {
    return;
  }
  checkLoading.value = true;
  try {
    selfCheck.value = await getScopeSelfCheck();
  } catch (error) {
    selfCheck.value = null;
    showError(error, '数据权限自检失败');
  } finally {
    checkLoading.value = false;
  }
}

function handleSearch(): void {
  pagination.current = 1;
  void loadList();
}

function handleReset(): void {
  query.role_name = '';
  handleSearch();
}

function handlePageChange(pageInfo: TablePageInfo): void {
  pagination.current = pageInfo.current;
  pagination.pageSize = pageInfo.pageSize;
  void loadList();
}

function openConfig(row: ScopeRow): void {
  current.value = row;
  drawerVisible.value = true;
}

/** 保存成功后刷新本行（不整表重载，保持滚动位置） */
function onSaved(): void {
  const target = current.value;
  if (!target) {
    return;
  }
  void getRoleScope(target.id)
    .then((scope) => {
      const row = rows.value.find((item) => item.id === target.id);
      if (row) {
        row.scope = scope;
      }
    })
    .catch(() => {
      // 回读失败忽略：面板本身已用保存响应更新过
    });
}

onMounted(() => {
  void loadList();
  void loadSelfCheck();
});
</script>

<style lang="less" scoped>
.data-scope-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

.check-card {
  :deep(.t-card__body) {
    padding: 16px;
  }

  &__head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 12px;
  }

  &__title {
    display: flex;
    align-items: center;
    gap: 6px;
    font-weight: 600;
    color: var(--td-text-color-primary);
  }
}

.check-grid {
  display: grid;
  grid-template-columns: repeat(auto-fit, minmax(180px, 1fr));
  gap: 12px;
  margin-bottom: 12px;
}

.check-item {
  display: flex;
  flex-direction: column;
  gap: 4px;
  padding: 10px 12px;
  border: 1px solid var(--td-component-stroke);
  border-radius: 6px;
  background: var(--td-bg-color-page);

  &__label {
    font-size: 12px;
    color: var(--td-text-color-secondary);
  }

  &__value {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 14px;
    color: var(--td-text-color-primary);
  }

  &__sub {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }
}

.check-roles {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: 8px;
  margin-bottom: 8px;
  font-size: 13px;

  &__label {
    color: var(--td-text-color-secondary);
  }
}

.check-tables {
  margin-top: 4px;

  &__head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 8px;
  }

  &__title {
    font-size: 13px;
    color: var(--td-text-color-secondary);
  }
}

.check-empty {
  padding: 20px 0;
  text-align: center;
  color: var(--td-text-color-placeholder);
}

.filter-card,
.table-card {
  :deep(.t-card__body) {
    padding: 16px;
  }
}

.toolbar {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  justify-content: space-between;
  gap: 8px;
  margin-bottom: 12px;

  &__title {
    font-weight: 600;
    color: var(--td-text-color-primary);
  }

  &__hint {
    font-size: 12px;
    color: var(--td-text-color-placeholder);
  }
}

.cell-name {
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
    font-size: 13px;
    color: var(--td-text-color-secondary);
  }
}
</style>
