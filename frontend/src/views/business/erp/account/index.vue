<template>
  <div class="account-page">
    <!-- ==================== ① 工具栏 ==================== -->
    <t-card :bordered="false" class="tool-card">
      <div class="tool-row">
        <t-button variant="outline" data-testid="btn-account-refresh" @click="loadAccounts()">
          <template #icon><t-icon name="refresh" /></template>
          刷新
        </t-button>

        <span class="tool-row__spacer" />

        <t-input
          v-model="filter.keyword"
          class="tool-row__keyword"
          placeholder="科目编码 / 名称"
          clearable
          data-testid="account-filter-keyword"
          @enter="applyFilter"
          @clear="applyFilter"
        />
        <t-select
          v-model="categoryModel"
          class="tool-row__category"
          :options="categoryOptions"
          placeholder="全部类型"
          clearable
          data-testid="account-filter-category"
          @change="applyFilter"
        />
        <t-select
          v-model="statusModel"
          class="tool-row__status"
          :options="statusOptions"
          placeholder="全部状态"
          clearable
          data-testid="account-filter-status"
          @change="applyFilter"
        />
        <t-button theme="primary" data-testid="btn-account-search" @click="applyFilter">查询</t-button>
        <t-button variant="outline" data-testid="btn-account-reset" @click="handleReset">重置</t-button>
      </div>
    </t-card>

    <!-- ==================== ② 科目树 ==================== -->
    <t-card :bordered="false" class="list-card">
      <div class="panel-head">
        <div class="panel-head__main">
          <span class="panel-head__title">会计科目</span>
          <span class="panel-head__meta" data-testid="account-meta">{{ metaText }}</span>
        </div>
        <div class="panel-head__side">
          <!-- ★ 本页只读：科目是财务专业配置（启停 / 增删要走管理员），界面上把这件事写明白 -->
          <span class="panel-head__tip" data-testid="account-readonly-tip">
            只读：科目由系统管理员维护
          </span>
          <t-button
            size="small"
            variant="text"
            data-testid="btn-account-expand-all"
            :disabled="!hasExpandable"
            @click="expandAll"
          >
            全部展开
          </t-button>
          <t-button
            size="small"
            variant="text"
            data-testid="btn-account-collapse-all"
            :disabled="expandedKeys.length === 0"
            @click="collapseAll"
          >
            全部收起
          </t-button>
        </div>
      </div>

      <t-alert
        v-if="apiMissing"
        theme="info"
        message="会计科目接口尚未上线"
        class="list-alert"
        data-testid="account-api-missing"
      >
        <template #description>
          <span class="mono">GET /api/business/erp/account/page</span> 返回 404。接口上线后本页无需改动。
        </template>
      </t-alert>

      <t-alert
        v-else-if="loadError"
        theme="error"
        :message="loadError"
        class="list-alert"
        data-testid="account-load-error"
      >
        <template #operation>
          <t-button size="small" variant="outline" @click="loadAccounts()">重试</t-button>
        </template>
      </t-alert>

      <!--
        树形表格必须用 `t-enhanced-table`（`t-table` 是 PrimaryTable，不认 tree）。
        数据一次拉全（`page_size = ACCOUNT_FETCH_LIMIT`），层级由 `buildAccountTree` 在前端拼。
      -->
      <t-enhanced-table
        v-if="treeRows.length > 0 || loading"
        row-key="id"
        :data="treeRows"
        :columns="columns"
        :loading="loading"
        :tree="treeConfig"
        :expanded-tree-nodes="expandedKeys"
        :hover="true"
        data-testid="account-table"
        @expanded-tree-nodes-change="handleExpandedKeysChange"
      >
        <template #account_code="{ row }">
          <span class="mono" :data-testid="`account-code-${row.account_code}`">{{ row.account_code }}</span>
        </template>

        <template #account_name="{ row }">
          <span :data-testid="`account-name-${row.account_code}`">{{ row.account_name }}</span>
          <t-tag
            v-if="row.level > 1"
            size="small"
            variant="light"
            theme="default"
            class="name-tag"
            data-testid="account-child-mark"
          >
            明细
          </t-tag>
        </template>

        <template #account_category="{ row }">
          <t-tag
            size="small"
            variant="light"
            :theme="accountCategoryTheme(row.account_category)"
            :data-testid="`account-category-${row.account_code}`"
          >
            {{ accountCategoryLabel(row.account_category) }}
          </t-tag>
        </template>

        <template #balance_direction="{ row }">
          <span class="muted" :data-testid="`account-direction-${row.account_code}`">
            {{ balanceDirectionLabel(row.balance_direction) }}
          </span>
        </template>

        <template #level="{ row }">
          <span class="mono muted">{{ accountLevelLabel(row.level) }}</span>
        </template>

        <template #status="{ row }">
          <t-tag
            size="small"
            variant="light"
            :theme="accountStatusTheme(row.status)"
            :data-testid="`account-status-${row.account_code}`"
          >
            {{ accountStatusLabel(row.status) }}
          </t-tag>
        </template>
      </t-enhanced-table>

      <div v-else class="panel-empty" data-testid="account-empty">
        <t-icon name="book" class="panel-empty__icon" />
        <p class="panel-empty__title">{{ emptyTitle }}</p>
        <p class="panel-empty__desc">
          {{ filtered ? '换个关键词或清空筛选条件再试' : '科目数据由系统管理员初始化（种子 4 条：1405 / 2202 / 2221 / 2221.01）' }}
        </p>
      </div>
    </t-card>
  </div>
</template>

<script setup lang="ts">
/**
 * 会计科目页（P4-C）——**只读树形**。
 *
 * # 字段来源（硬红线 1：以库列 / DTO 为准，不猜）
 *
 * `psql \d biz_account`：`account_code / account_name / parent_id / level / account_category /
 * balance_direction / is_leaf / aux_dimensions / status(smallint) / remark`；
 * 出参 `dto.rs::AccountRes` 同名 + `id / created_at / updated_at`。
 * 所以列里是 `account_code`，不是卡面草稿里的 `code`。
 *
 * # 为什么一次拉全量（`page_size = 500`）而不是服务端分页
 *
 * 树要「父 + 全部子」才成立：分页取第 1 页时，第 2 页的子科目父节点不在本页 →
 * 会被当成孤儿挂到根上，树结构就假了。科目是低频主数据（种子 4 条，企业户几百条），
 * 一次拉全 + 前端拼树 + 前端过滤，既正确又快（少 3 次往返）。
 * 真到上万条该走「懒加载子树」，那是后端加 `/account/tree` 的活，不在本卡。
 *
 * # 只读（硬红线 2）
 *
 * 没有新建 / 编辑 / 启停按钮：财务科目是专业配置，误点一次会让凭证分录挂错科目。
 * 页头用一行字说明「由系统管理员维护」，省得财务来问「为什么这页不能改」。
 */
import { computed, onMounted, reactive, ref } from 'vue';
import type { PrimaryTableCol } from 'tdesign-vue-next';
import { ApiError } from '@/api/request';
import { fetchAccountPage, type AccountRow } from '@/api/business/erp';
import {
  ACCOUNT_CATEGORY_OPTIONS,
  ACCOUNT_STATUS_OPTIONS,
  accountCategoryLabel,
  accountCategoryTheme,
  accountLevelLabel,
  accountStatusLabel,
  accountStatusTheme,
  accountTreeDepth,
  accountTreeMeta,
  balanceDirectionLabel,
  buildAccountTree,
  emptyAccountFilter,
  expandableAccountIds,
  filterAccountTree,
  initialExpandedAccountIds,
  isAccountFiltered,
  TREE_INDENT,
  type AccountFilterState,
  type AccountTreeNode,
} from './account-tree';

/**
 * 一次拉多少条科目。
 *
 * 500 是「足够覆盖真实规模（一般 300~400 个科目）又不会拖慢首屏」的经验值；
 * 后端 `page_size` 上限若更小，`total > list.length` 时页头会提示「只显示前 N 条」。
 */
const ACCOUNT_FETCH_LIMIT = 500;

/** 表格列（`account_code` 在最左 —— 树形箭头挂在这一列，财务看科目先看编码） */
const columns: PrimaryTableCol[] = [
  { colKey: 'account_code', title: '科目编码', width: 170 },
  { colKey: 'account_name', title: '科目名称', minWidth: 220, ellipsis: true },
  { colKey: 'account_category', title: '科目类型', width: 110 },
  { colKey: 'balance_direction', title: '余额方向', width: 100, align: 'center' },
  { colKey: 'level', title: '层级', width: 90 },
  { colKey: 'status', title: '是否启用', width: 110 },
];

/** 树形配置：箭头挂在 `account_code` 列（按 colKey 反查下标，插列也不会错位） */
const arrowIndex = columns.findIndex((col) => col.colKey === 'account_code');
const treeConfig = { treeNodeColumnIndex: arrowIndex >= 0 ? arrowIndex : 0, indent: TREE_INDENT };

const categoryOptions = ACCOUNT_CATEGORY_OPTIONS.map((item) => ({ ...item }));
const statusOptions = ACCOUNT_STATUS_OPTIONS.map((item) => ({ ...item }));

// ==================== 数据 ====================

/** 后端原始清单（未过滤） */
const allRows = ref<AccountRow[]>([]);
/** 后端 total（> allRows.length 说明只拿到了前 500 条） */
const serverTotal = ref(0);
const loading = ref(false);
const loadError = ref('');
/** 接口未注册（404）：不报红，改成信息条 + 空态 */
const apiMissing = ref(false);

const filter = reactive<AccountFilterState>(emptyAccountFilter());

/**
 * `t-select` 的 `v-model` 类型是 `string | number | … | undefined`，**不收 `null`**；
 * 而纯逻辑里「全部」统一用 `null` / `''`（与后端 `Option<T>` 同构，单测也好写）。
 * 这两个双向 computed 就是桥：清空 → `null` / `''`，选中 → 原值。逻辑层不必为 UI 妥协。
 */
const statusModel = computed<number | string | undefined>({
  get: () => (filter.status === null ? undefined : filter.status),
  set: (value) => {
    filter.status = typeof value === 'number' ? value : null;
  },
});

/** 类型下拉的桥（同上：`''` = 全部） */
const categoryModel = computed<string | undefined>({
  get: () => filter.account_category || undefined,
  set: (value) => {
    filter.account_category = (typeof value === 'string' ? value : '') as AccountFilterState['account_category'];
  },
});

const filtered = computed(() => isAccountFiltered(filter));

/** 全量树（未过滤）：给「全部展开 / 收起」和 meta 用 */
const fullTree = computed<AccountTreeNode[]>(() => buildAccountTree(allRows.value));

/** 过滤结果（命中 + 祖先） */
const filterResult = computed(() => filterAccountTree(fullTree.value, filter));

/** 表格数据 */
const treeRows = computed<AccountTreeNode[]>(() => filterResult.value.tree);

/** 当前可见的树深（验收②「3 层结构正确展开」在页面上就能量到） */
const visibleDepth = computed(() => accountTreeDepth(treeRows.value));

const hasExpandable = computed(() => expandableAccountIds(treeRows.value).length > 0);

const metaText = computed(() => {
  if (loading.value && allRows.value.length === 0) {
    return '加载中…';
  }
  const base = accountTreeMeta(treeRows.value);
  const parts = [base];
  if (filtered.value) {
    parts.push(`筛选命中 ${filterResult.value.matchedCount} 条`);
  }
  if (serverTotal.value > allRows.value.length) {
    parts.push(`后端共 ${serverTotal.value} 条，仅载入前 ${allRows.value.length} 条`);
  }
  if (visibleDepth.value > 1) {
    parts.push(`${visibleDepth.value} 层`);
  }
  return parts.join(' · ');
});

const emptyTitle = computed(() => {
  if (filtered.value) {
    return '没有匹配的科目';
  }
  return apiMissing.value ? '接口未上线，暂无科目数据' : '暂无科目数据';
});

/** 受控展开态（`t-enhanced-table` 的 `expandedTreeNodes`） */
const expandedKeys = ref<string[]>([]);

// ==================== 行为 ====================

/** 错误 → 文案 */
function messageOf(error: unknown): string {
  if (error instanceof ApiError) {
    return error.message;
  }
  return error instanceof Error ? error.message : String(error);
}

/** 未注册路由返回 HTTP 404（`{code:404}`），业务 404 是 `10002`；两者都算「接口没上线」 */
function isApiMissing(error: unknown): boolean {
  if (!(error instanceof ApiError)) {
    return false;
  }
  if (error.code === 404) {
    return true;
  }
  return error.message.includes('404') || error.message.includes('请求的资源不存在');
}

/**
 * 拉全量科目并重建树。
 *
 * 展开态策略：**首屏 / 刷新后重置为「全展开」**（科目数据小，全展开才好核对结构）；
 * 只有用户手动折叠过的节点会在下一次「筛选」时按过滤结果重新展开 —— 见 `applyFilter`。
 */
async function loadAccounts(): Promise<void> {
  loading.value = true;
  loadError.value = '';
  try {
    const data = await fetchAccountPage({ page: 1, page_size: ACCOUNT_FETCH_LIMIT });
    apiMissing.value = false;
    allRows.value = data.list ?? [];
    serverTotal.value = data.total ?? allRows.value.length;
    expandedKeys.value = initialExpandedAccountIds(buildAccountTree(allRows.value));
  } catch (error) {
    allRows.value = [];
    serverTotal.value = 0;
    expandedKeys.value = [];
    apiMissing.value = isApiMissing(error);
    loadError.value = apiMissing.value ? '' : messageOf(error);
  } finally {
    loading.value = false;
  }
}

/** 点「查询」/ 改筛选：过滤后自动展开「有下级的命中节点」 */
function applyFilter(): void {
  expandedKeys.value = expandableAccountIds(filterResult.value.tree);
}

/** 重置筛选（不动数据，只清条件） */
function handleReset(): void {
  Object.assign(filter, emptyAccountFilter());
  expandedKeys.value = initialExpandedAccountIds(treeRows.value);
}

/** 全部展开 */
function expandAll(): void {
  expandedKeys.value = expandableAccountIds(treeRows.value);
}

/** 全部收起 */
function collapseAll(): void {
  expandedKeys.value = [];
}

/** 表格受控展开态镜像（折叠 / 展开由表格判定，这里只同步） */
function handleExpandedKeysChange(keys: Array<string | number>): void {
  expandedKeys.value = keys.map((key) => String(key));
}

onMounted(() => {
  void loadAccounts();
});
</script>

<style scoped lang="less">
.account-page {
  display: flex;
  flex-direction: column;
  gap: 12px;
}

/* ==================== 工具栏 ==================== */

.tool-card {
  flex: 0 0 auto;
}

.tool-row {
  display: flex;
  align-items: center;
  gap: 12px;
  flex-wrap: wrap;
}

.tool-row__spacer {
  flex: 1 1 auto;
}

.tool-row__keyword {
  width: 220px;
}

.tool-row__category {
  width: 130px;
}

.tool-row__status {
  width: 130px;
}

/* ==================== 科目树 ==================== */

.list-card {
  flex: 1 1 auto;
  min-width: 0;
}

.panel-head {
  display: flex;
  align-items: baseline;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 10px;
  flex-wrap: wrap;
}

.panel-head__main {
  display: flex;
  align-items: baseline;
  gap: 10px;
  flex-wrap: wrap;
}

.panel-head__title {
  font-size: 14px;
  font-weight: 600;
  color: var(--td-text-color-primary);
}

.panel-head__meta {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.panel-head__side {
  display: flex;
  align-items: center;
  gap: 6px;
}

.panel-head__tip {
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}

.list-alert {
  margin-bottom: 10px;
}

.mono {
  font-family: var(--td-font-family-mono, ui-monospace, monospace);
}

.muted {
  color: var(--td-text-color-placeholder);
}

.name-tag {
  margin-left: 6px;
}

.panel-empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 4px;
  padding: 24px 0;
}

.panel-empty__icon {
  font-size: 28px;
  color: var(--td-text-color-placeholder);
}

.panel-empty__title {
  margin: 0;
  font-size: 14px;
  color: var(--td-text-color-secondary);
}

.panel-empty__desc {
  margin: 0;
  font-size: 12px;
  color: var(--td-text-color-placeholder);
}
</style>
